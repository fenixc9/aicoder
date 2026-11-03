use super::*;
use aicoder_core::{
    composition::*,
    tools::{ToolFailure, ToolSuccess},
};
use rquickjs::{
    Context, Ctx, Exception, Function, Object, Promise, Runtime, String as JsString,
    Value as JsValue, function::Opt, promise::PromiseState,
};
use serde::{Deserialize, Serialize};
use std::{
    cell::RefCell,
    collections::BTreeMap,
    rc::Rc,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc,
    },
    thread,
    time::Instant,
};
use tokio::sync::{Notify, mpsc as async_mpsc};
#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PublicState {
    version: u32,
    values: BTreeMap<String, Value>,
}
struct End {
    done: Arc<AtomicBool>,
    changed: Arc<Notify>,
    active: Arc<AtomicUsize>,
}
impl Drop for End {
    fn drop(&mut self) {
        self.active.fetch_sub(1, Ordering::AcqRel);
        self.done.store(true, Ordering::Release);
        self.changed.notify_waiters();
    }
}
struct VmProgram {
    events: async_mpsc::Receiver<ProgramEvent>,
    replies: mpsc::SyncSender<ProgramResponse>,
    stop: Arc<AtomicBool>,
    done: Arc<AtomicBool>,
    changed: Arc<Notify>,
    owner: Option<thread::JoinHandle<()>>,
}
#[async_trait]
impl ToolProgram for VmProgram {
    async fn next(&mut self) -> Result<ProgramEvent, ToolFailure> {
        self.events.recv().await.ok_or_else(|| {
            ToolFailure::new(
                "code_mode_worker",
                "VM owner ended without a program result",
            )
        })
    }
    fn respond(&mut self, response: ProgramResponse) -> Result<(), ToolFailure> {
        self.replies
            .try_send(response)
            .map_err(|_| ToolFailure::new("code_mode_queue", "VM reply queue is full or closed"))
    }
    fn cancel(&mut self) {
        self.stop.store(true, Ordering::Release);
        let _ = self.replies.try_send(ProgramResponse {
            id: 0,
            content: Value::Null,
            is_error: true,
        });
    }
    async fn shutdown(&mut self) -> Result<(), ToolFailure> {
        self.cancel();
        loop {
            let changed = self.changed.notified();
            if self.done.load(Ordering::Acquire) {
                break;
            }
            changed.await;
        }
        if let Some(owner) = self.owner.take() {
            owner
                .join()
                .map_err(|_| ToolFailure::new("code_mode_worker", "VM owner panicked"))?;
        }
        Ok(())
    }
}
impl Drop for VmProgram {
    fn drop(&mut self) {
        self.cancel();
        if let Some(owner) = self.owner.take() {
            if owner.is_finished() {
                let _ = owner.join();
            } else {
                let _ = thread::Builder::new()
                    .name("aicoder-js-reaper".into())
                    .spawn(move || {
                        let _ = owner.join();
                    });
            }
        }
    }
}
pub(super) fn start(
    code: String,
    context: CompositionContext,
    config: CodeModeConfig,
    active: Arc<AtomicUsize>,
) -> Result<Box<dyn ToolProgram>, ToolFailure> {
    let state = if let Some(value) = context.state {
        let state: PublicState = serde_json::from_value(value).map_err(|_| {
            ToolFailure::new("code_mode_state", "Saved public state is incompatible")
        })?;
        if state.version != 1 || !valid_state(&state, config.limits.max_state_bytes) {
            return Err(ToolFailure::new(
                "code_mode_state",
                "Saved public state exceeds its declared limits",
            ));
        }
        state
    } else {
        PublicState {
            version: 1,
            ..Default::default()
        }
    };
    active
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
            (n < config.max_workers).then_some(n + 1)
        })
        .map_err(|_| ToolFailure::new("code_mode_busy", "VM concurrency is full"))?;
    let stop = Arc::new(AtomicBool::new(false));
    let done = Arc::new(AtomicBool::new(false));
    let changed = Arc::new(Notify::new());
    let end = End {
        done: done.clone(),
        changed: changed.clone(),
        active,
    };
    let (sent, events) = async_mpsc::channel(64);
    let (replies, receive) = mpsc::sync_channel(64);
    let worker_stop = stop.clone();
    let owner = thread::Builder::new()
        .name("aicoder-codemode".into())
        .stack_size(2 * 1024 * 1024)
        .spawn(move || {
            let _end = end;
            if let Err(error) = run(
                code,
                context.tools,
                state,
                config,
                worker_stop,
                receive,
                &sent,
            ) {
                let _ = sent.try_send(ProgramEvent::Failed(error));
            }
        })
        .map_err(|_| ToolFailure::new("code_mode_worker", "Cannot start VM owner"))?;
    Ok(Box::new(VmProgram {
        events,
        replies,
        stop,
        done,
        changed,
        owner: Some(owner),
    }))
}
fn fits(value: &impl Serialize, limit: usize) -> bool {
    struct Count(usize);
    impl std::io::Write for Count {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > self.0 {
                return Err(std::io::Error::other("VM JSON budget"));
            }
            self.0 -= bytes.len();
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    serde_json::to_writer(&mut Count(limit), value).is_ok()
}
fn valid_state(state: &PublicState, limit: usize) -> bool {
    state.values.len() <= 64
        && state
            .values
            .keys()
            .all(|key| !key.is_empty() && key.len() <= 64 && !key.chars().any(char::is_control))
        && fits(state, limit)
}
fn string<'js>(ctx: &Ctx<'js>, value: JsString<'js>, max: usize) -> rquickjs::Result<String> {
    let bytes = value.clone().to_cstring()?;
    if bytes.len() > max {
        return Err(Exception::throw_message(
            ctx,
            "String exceeds its byte budget",
        ));
    }
    drop(bytes);
    value.to_string()
}
fn json_value<'js>(ctx: &Ctx<'js>, value: JsValue<'js>, max: usize) -> rquickjs::Result<Value> {
    let text = ctx
        .json_stringify(value)?
        .ok_or_else(|| Exception::throw_type(ctx, "Value is not JSON serializable"))?;
    let text = string(ctx, text, max)?;
    serde_json::from_str(&text).map_err(|_| Exception::throw_type(ctx, "Value is not bounded JSON"))
}
struct PendingReply<'js> {
    resolve: Function<'js>,
    reject: Function<'js>,
    envelope: bool,
    image: bool,
}
struct Host<'js> {
    pending: BTreeMap<u64, PendingReply<'js>>,
    sequence: u64,
    items: Vec<Value>,
    console: Vec<String>,
    bytes: usize,
    state: PublicState,
    dirty: bool,
    exit: bool,
    config: CodeModeConfig,
}
impl Host<'_> {
    fn output(&mut self, value: Value, console: bool) -> rquickjs::Result<()> {
        let encoded = serde_json::to_vec(&value).map_err(|_| rquickjs::Error::Unknown)?;
        if self.items.len() + self.console.len() >= self.config.max_output_items
            || encoded.len() > self.config.max_output_bytes.saturating_sub(self.bytes)
        {
            return Err(rquickjs::Error::Unknown);
        }
        self.bytes += encoded.len();
        if console {
            self.console.push(value.as_str().unwrap_or_default().into());
        } else {
            self.items.push(value);
        }
        Ok(())
    }
    fn snapshot(&self, status: &str) -> Value {
        json!({"status":status,"items":self.items,"console":self.console})
    }
    fn failed(&self, message: &str) -> ToolFailure {
        ToolFailure::new(
            "code_mode_script",
            aicoder_core::redaction::sanitize_text(message, 4096),
        )
        .with_details(self.snapshot("failed"))
    }
}
fn run(
    code: String,
    tools: Vec<ToolDefinition>,
    state: PublicState,
    config: CodeModeConfig,
    stop: Arc<AtomicBool>,
    replies: mpsc::Receiver<ProgramResponse>,
    events: &async_mpsc::Sender<ProgramEvent>,
) -> Result<(), ToolFailure> {
    let runtime =
        Runtime::new().map_err(|_| ToolFailure::new("code_mode_vm", "Cannot create VM"))?;
    runtime.set_memory_limit(config.memory_bytes);
    runtime.set_max_stack_size(config.stack_bytes);
    let deadline = Instant::now() + config.limits.duration;
    let ticks = Arc::new(AtomicUsize::new(0));
    let interrupt = ticks.clone();
    let interrupted = stop.clone();
    let quota = config.interrupt_checks;
    runtime.set_interrupt_handler(Some(Box::new(move || {
        interrupted.load(Ordering::Acquire)
            || Instant::now() >= deadline
            || interrupt.fetch_add(1, Ordering::AcqRel) >= quota
    })));
    let context = Context::full(&runtime)
        .map_err(|_| ToolFailure::new("code_mode_vm", "Cannot create VM context"))?;
    context.with(|ctx| {
        // Capture pristine intrinsics before user code. Inspect only native Error data
        // properties: formatting an exception must not invoke a user getter or Proxy trap.
        let formatter: Function = ctx.eval("((own, has) => value => { const d=own(value,'message'); return d && has(d,'value') && typeof d.value==='string' ? d.value : 'JavaScript exception'; })(Object.getOwnPropertyDescriptor,Object.hasOwn)").map_err(|_|ToolFailure::new("code_mode_vm","Cannot create error formatter"))?;
        let host = Rc::new(RefCell::new(Host {
            pending: BTreeMap::new(),
            sequence: 0,
            items: Vec::new(),
            console: Vec::new(),
            bytes: 0,
            state,
            dirty: false,
            exit: false,
            config: config.clone(),
        }));
        let outcome = (|| {
            if let Err(error) = install(ctx.clone(), &tools, host.clone(), events.clone()) {
                return Err(host
                    .borrow()
                    .failed(&format!("VM bindings failed: {error}")));
            }
            let source = format!("(async function(){{\n{code}\n}})()");
            let promise = match ctx.eval::<Promise, _>(source) {
                Ok(promise) => promise,
                Err(_) => { let message=exception(&ctx,&formatter); return Err(host.borrow().failed(&message)); },
            };
            let mut jobs = 0usize;
            loop {
                if stop.load(Ordering::Acquire) {
                    return Err(host.borrow().failed("Script was cancelled"));
                }
                if Instant::now() >= deadline {
                    return Err(host.borrow().failed("Script exceeded its wall deadline"));
                }
                while ctx.execute_pending_job() {
                    jobs += 1;
                    if jobs > config.max_jobs
                        || stop.load(Ordering::Acquire)
                        || Instant::now() >= deadline
                    {
                        return Err(host
                            .borrow()
                            .failed("Script job/cancellation budget reached"));
                    }
                    if host.borrow().exit {
                        break;
                    }
                }
                if host.borrow().exit {
                    break;
                }
                match promise.state() {
                    PromiseState::Pending => {}
                    PromiseState::Rejected => {
                        let _ = promise.result::<JsValue>();
                        let message = exception(&ctx,&formatter);
                        return Err(host.borrow().failed(&message));
                    }
                    PromiseState::Resolved => {
                        if let Some(Ok(value)) = promise.result::<JsValue>()
                            && !value.is_undefined()
                        {
                            let data = if let Some(text) = value.as_string() {
                                Value::String(
                                    string(&ctx, text.clone(), config.max_output_bytes).map_err(
                                        |_| {
                                            host.borrow()
                                                .failed("Return value exceeds output budget")
                                        },
                                    )?,
                                )
                            } else {
                                json_value(&ctx, value, config.max_output_bytes).map_err(|_| {
                                    host.borrow().failed("Return value is not bounded JSON")
                                })?
                            };
                            let accepted = host.borrow_mut().output(data,false);
                            accepted.map_err(|_|host.borrow().failed("Script output budget exceeded"))?;
                        }
                        break;
                    }
                }
                match replies.recv_timeout(Duration::from_millis(5)) {
                    Ok(response) => {
                        if response.id == 0 && stop.load(Ordering::Acquire) {
                            continue;
                        }
                        let pending = host.borrow_mut().pending.remove(&response.id);
                        let Some(pending) = pending else {
                            return Err(host.borrow().failed("Unknown or duplicate host result"));
                        };
                        if !fits(&response.content, config.limits.max_result_bytes + 16384) {
                            return Err(host
                                .borrow()
                                .failed("Host response exceeds VM transfer budget"));
                        }
                        let mut transfer = response.content.clone();
                        if pending.envelope && !response.is_error {
                            let images = transfer.get("artifacts").and_then(Value::as_array).into_iter().flatten()
                                .filter_map(|value|serde_json::from_value::<aicoder_core::artifacts::ArtifactRef>(value.clone()).ok())
                                .filter(|reference|reference.validate().is_ok() && reference.is_image()).collect::<Vec<_>>();
                            if !images.is_empty() { transfer["_image_artifacts"] = json!(images); }
                        }
                        if !fits(&transfer, config.limits.max_result_bytes + 16384) {
                            return Err(host.borrow().failed("Host image metadata exceeds VM transfer budget"));
                        }
                        let value = ctx
                            .json_parse(serde_json::to_vec(&transfer).expect("JSON value"))
                            .map_err(|_| {
                                host.borrow().failed("Host response cannot be imported")
                            })?;
                        let result = if response.is_error {
                            let error = Object::new(ctx.clone()).map_err(|_|host.borrow().failed("Cannot create tool error"))?;
                            error.set_prototype(None).map_err(|_|host.borrow().failed("Cannot scope tool error"))?;
                            error.set("name","ToolError").map_err(|_|host.borrow().failed("Cannot set tool error name"))?;
                            error.set("message",response.content["error"]["message"].as_str().unwrap_or("Tool call failed")).map_err(|_|host.borrow().failed("Cannot attach tool error text"))?;
                            error.set("code",response.content["error"]["code"].as_str().unwrap_or("tool_failed")).map_err(|_|host.borrow().failed("Cannot attach tool error code"))?;
                            error.set("details",value).map_err(|_|host.borrow().failed("Cannot attach tool error"))?;
                            pending.reject.call::<_, ()>((error,))
                        } else if pending.image {
                                pending.resolve.call::<_, ()>((JsValue::new_undefined(ctx.clone()),))
                            } else if pending.envelope {
                                pending.resolve.call::<_, ()>((value,))
                            } else {
                                let object = value.as_object().ok_or_else(|| host.borrow().failed("Host tool envelope is invalid"))?;
                                let output: JsValue = object.get("output").map_err(|_| host.borrow().failed("Host tool output is absent"))?;
                                pending.resolve.call::<_, ()>((output,))
                        };
                        if result.is_err() {
                            let message = exception(&ctx,&formatter);
                        return Err(host.borrow().failed(&message));
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                        return Err(host.borrow().failed("Host response channel closed"));
                    }
                }
            }
            let host = host.borrow();
            let state = host
                .dirty
                .then(|| serde_json::to_value(&host.state).expect("public state"));
            events
                .try_send(ProgramEvent::Finished(ProgramOutput {
                    result: ToolSuccess::new(host.snapshot("completed")),
                    state,
                }))
                .map_err(|_| host.failed("Program result queue is full or closed"))?;
            Ok(())
        })();
        host.borrow_mut().pending.clear();
        outcome
    })
}
fn exception<'js>(ctx: &Ctx<'js>, formatter: &Function<'js>) -> String {
    let value = ctx.catch();
    if value.as_exception().is_some() {
        formatter
            .call::<_, JsString>((value,))
            .ok()
            .and_then(|text| string(ctx, text, 4096).ok())
            .unwrap_or_else(|| "JavaScript execution failed or hit a resource limit".into())
    } else if let Some(text) = value.as_string() {
        string(ctx, text.clone(), 4096)
            .unwrap_or_else(|_| "JavaScript exception exceeds its budget".into())
    } else {
        "JavaScript exception or resource limit".into()
    }
}

fn tool_alias(name: &str) -> String {
    let mut alias = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '_' | '$') {
                c
            } else {
                '_'
            }
        })
        .collect::<String>();
    if alias.as_bytes().first().is_some_and(u8::is_ascii_digit) {
        alias.insert(0, '_');
    }
    alias
}
fn tool_function<'js>(
    ctx: &Ctx<'js>,
    name: String,
    state: Rc<RefCell<Host<'js>>>,
    sent: async_mpsc::Sender<ProgramEvent>,
    envelope: bool,
    image: bool,
) -> rquickjs::Result<Function<'js>> {
    Function::new(
        ctx.clone(),
        move |ctx: Ctx<'js>, argument: Opt<JsValue<'js>>| -> rquickjs::Result<Promise<'js>> {
            let (promise, resolve, reject) = ctx.promise()?;
            let mut host = state.borrow_mut();
            host.sequence += 1;
            if host.sequence > host.config.limits.max_calls as u64 || host.pending.len() >= 64 {
                return Err(Exception::throw_message(&ctx, "Tool call budget exceeded"));
            }
            let id = host.sequence;
            let limit = host.config.limits.max_request_bytes;
            drop(host);
            let argument = match argument.0 {
                Some(value) => value,
                None => Object::new(ctx.clone())?.into_value(),
            };
            let args = json_value(&ctx, argument, limit)?;
            let mut host = state.borrow_mut();
            host.pending.insert(
                id,
                PendingReply {
                    resolve,
                    reject,
                    envelope,
                    image,
                },
            );
            let call = ProgramCall {
                id,
                tool: name.clone(),
                arguments: args,
            };
            let event = if image {
                ProgramEvent::ImageCall(call)
            } else {
                ProgramEvent::Call(call)
            };
            if sent.try_send(event).is_err() {
                host.pending.remove(&id);
                return Err(Exception::throw_message(
                    &ctx,
                    "Tool intent queue is full or closed",
                ));
            }
            Ok(promise)
        },
    )
}
fn install<'js>(
    ctx: Ctx<'js>,
    tools: &[ToolDefinition],
    host: Rc<RefCell<Host<'js>>>,
    events: async_mpsc::Sender<ProgramEvent>,
) -> rquickjs::Result<()> {
    let globals = ctx.globals();
    let methods = Object::new(ctx.clone())?;
    methods.set_prototype(None)?;
    let names = tools
        .iter()
        .map(|tool| tool.name.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    let mut aliases = std::collections::BTreeMap::new();
    for tool in tools {
        let alias = tool_alias(&tool.name);
        if (alias != tool.name && names.contains(alias.as_str()))
            || aliases.insert(alias, tool.name.as_str()).is_some()
        {
            return Err(Exception::throw_type(
                &ctx,
                "Tool aliases collide; select an explicit host subset",
            ));
        }
    }
    for tool in tools {
        let function = tool_function(
            &ctx,
            tool.name.clone(),
            host.clone(),
            events.clone(),
            false,
            false,
        )?;
        function.set(
            "result",
            tool_function(
                &ctx,
                tool.name.clone(),
                host.clone(),
                events.clone(),
                true,
                false,
            )?,
        )?;
        let alias = tool_alias(&tool.name);
        if alias != tool.name {
            methods.set(alias, function.clone())?;
        }
        methods.set(tool.name.as_str(), function)?;
    }
    globals.set("tools", methods)?;
    let image_tool = host
        .borrow()
        .config
        .image_tool
        .clone()
        .filter(|name| names.contains(name.as_str()));
    if let Some(name) = image_tool {
        globals.set(
            "__image",
            tool_function(&ctx, name, host.clone(), events.clone(), false, true)?,
        )?;
    } else {
        globals.set(
            "__image",
            Function::new(ctx.clone(), |ctx: Ctx<'js>| -> rquickjs::Result<()> {
                Err(Exception::throw_message(
                    &ctx,
                    "Image output tool is not in the host program catalog",
                ))
            })?,
        )?;
    }

    let output = host.clone();
    globals.set(
        "text",
        Function::new(
            ctx.clone(),
            move |ctx: Ctx<'js>, value: Opt<JsValue<'js>>| -> rquickjs::Result<()> {
                let value = value
                    .0
                    .unwrap_or_else(|| JsValue::new_undefined(ctx.clone()));
                let limit = output.borrow().config.max_output_bytes;
                let data = if value.is_undefined() {
                    Value::Null
                } else if let Some(text) = value.as_string() {
                    Value::String(string(&ctx, text.clone(), limit)?)
                } else {
                    json_value(&ctx, value, limit)?
                };
                output
                    .borrow_mut()
                    .output(data, false)
                    .map_err(|_| Exception::throw_message(&ctx, "Script output budget exceeded"))
            },
        )?,
    )?;
    let console = host.clone();
    globals.set(
        "__console",
        Function::new(
            ctx.clone(),
            move |ctx: Ctx<'js>, values: JsValue<'js>| -> rquickjs::Result<()> {
                let limit = console.borrow().config.max_output_bytes;
                let data = json_value(&ctx, values, limit)?;
                let values = data
                    .as_array()
                    .ok_or_else(|| Exception::throw_type(&ctx, "Console expects bounded values"))?;
                let text = values
                    .iter()
                    .map(|value| {
                        value
                            .as_str()
                            .map(str::to_string)
                            .unwrap_or_else(|| value.to_string())
                    })
                    .collect::<Vec<_>>()
                    .join(" ");
                console
                    .borrow_mut()
                    .output(Value::String(text), true)
                    .map_err(|_| Exception::throw_message(&ctx, "Console budget exceeded"))
            },
        )?,
    )?;
    let storage = host.clone();
    globals.set(
        "store",
        Function::new(
            ctx.clone(),
            move |ctx: Ctx<'js>,
                  key: JsString<'js>,
                  value: Opt<JsValue<'js>>|
                  -> rquickjs::Result<()> {
                let key = string(&ctx, key, 64)?;
                if key.is_empty() || key.chars().any(char::is_control) {
                    return Err(Exception::throw_type(&ctx, "Invalid public state key"));
                }
                let limit = storage.borrow().config.limits.max_state_bytes;
                let value = match value.0 {
                    Some(value) if !value.is_undefined() => Some(json_value(&ctx, value, limit)?),
                    _ => None,
                };
                let mut host = storage.borrow_mut();
                let mut next = PublicState {
                    version: 1,
                    values: host.state.values.clone(),
                };
                if let Some(value) = value {
                    next.values.insert(key, value);
                } else {
                    next.values.remove(&key);
                }
                if !valid_state(&next, limit) {
                    return Err(Exception::throw_message(
                        &ctx,
                        "Public state budget exceeded",
                    ));
                }
                host.state = next;
                host.dirty = true;
                Ok(())
            },
        )?,
    )?;
    let storage = host.clone();
    globals.set(
        "load",
        Function::new(
            ctx.clone(),
            move |ctx: Ctx<'js>, key: JsString<'js>| -> rquickjs::Result<JsValue<'js>> {
                let key = string(&ctx, key, 64)?;
                match storage.borrow().state.values.get(&key) {
                    Some(value) => ctx.json_parse(serde_json::to_vec(value).expect("public JSON")),
                    None => Ok(JsValue::new_undefined(ctx)),
                }
            },
        )?,
    )?;
    let exit = host;
    globals.set(
        "exit",
        Function::new(ctx.clone(), move |ctx: Ctx<'js>| -> rquickjs::Result<()> {
            exit.borrow_mut().exit = true;
            Err(Exception::throw_message(&ctx, "Script exited"))
        })?,
    )?;
    globals.set(
        "__catalog",
        serde_json::to_string(
            &tools
                .iter()
                .map(|tool| {
                    json!({
                        "name": tool.name, "alias": tool_alias(&tool.name),
                        "description": tool.description, "parameters": tool.parameters,
                    })
                })
                .collect::<Vec<_>>(),
        )
        .expect("tool definitions"),
    )?;
    ctx.eval::<(), _>(include_str!("shim.js"))?;
    Ok(())
}

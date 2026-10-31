use crate::{
    WebHost,
    host::{MAX_BODY, MAX_ID},
};
use aicoder_core::{
    TurnExecutionContext,
    extensions::{CommandContext, CommandOutput, CommandRequest},
    session::RunSession,
    ui::UiHost,
};
use futures::FutureExt;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use tokio::{sync::Notify, task::JoinHandle};
pub(crate) struct CommandJob {
    pub id: String,
    pub request_id: u64,
    pub ui: UiHost,
    context: TurnExecutionContext,
    result: Mutex<Option<Value>>,
    owner: Mutex<Option<JoinHandle<()>>>,
    changed: Notify,
}
impl CommandJob {
    pub fn active(&self) -> bool {
        self.result.lock().expect("command result").is_none()
    }
    pub fn cancel(&self, reason: &str) {
        self.context.cancel(reason.to_string());
        self.ui.close();
    }
    pub fn snapshot(&self) -> Value {
        json!({"version":1,"id":self.id,"request_id":self.request_id,"ui":self.ui.snapshot(),"result":*self.result.lock().expect("command result")})
    }
    pub async fn join(&self) {
        let owner = self.owner.lock().expect("command owner").take();
        if let Some(owner) = owner {
            let _ = owner.await;
        } else {
            loop {
                let changed = self.changed.notified();
                if !self.active() {
                    break;
                }
                changed.await;
            }
        }
    }
    fn finish(&self, result: Value) {
        let mut stored = self.result.lock().expect("command result");
        if stored.is_none() {
            self.ui.close();
            *stored = Some(result);
            self.changed.notify_waiters();
        }
    }
}
struct Complete(Arc<CommandJob>);
impl Drop for Complete {
    fn drop(&mut self) {
        if self.0.active() {
            self.0
                .finish(json!({"status":"failed","message":"Command ended without a result"}));
        }
    }
}
impl WebHost {
    pub fn commands(&self) -> Vec<aicoder_core::extensions::CommandSpec> {
        self.0
            .options
            .extensions
            .as_ref()
            .map(|extensions| extensions.commands())
            .unwrap_or_default()
    }
    pub(crate) fn command(&self, id: &str) -> Option<Arc<CommandJob>> {
        self.0
            .state
            .lock()
            .expect("Web state")
            .command
            .as_ref()
            .filter(|job| job.id == id)
            .cloned()
    }
    pub(crate) fn cancel_command(&self, id: &str) -> bool {
        let state = self.0.state.lock().expect("Web state");
        if let Some(job) = &state.command
            && job.id == id
            && job.active()
        {
            job.cancel("Web command cancelled");
            true
        } else {
            false
        }
    }
    pub(crate) fn start_command(
        &self,
        id: u64,
        name: String,
        arguments: Value,
    ) -> Result<Arc<CommandJob>, (&'static str, &'static str)> {
        let mut state = self.0.state.lock().expect("Web state");
        if state.closing {
            return Err(("closing", "Host is shutting down"));
        }
        if id == 0 || id > MAX_ID || id <= state.last_id {
            return Err(("stale_id", "Request IDs must strictly increase"));
        }
        state.last_id = id;
        if state.command.as_ref().is_some_and(|job| job.active())
            || state
                .current
                .as_ref()
                .is_some_and(|run| run.completion.lock().expect("Web completion").is_none())
        {
            return Err(("busy", "Wait for the current operation to finish"));
        }
        let extensions = self
            .0
            .options
            .extensions
            .clone()
            .ok_or(("unknown_command", "Host has no registered commands"))?;
        if !extensions.commands().iter().any(|spec| spec.name == name) {
            return Err(("unknown_command", "Command is not registered by this host"));
        }
        if serde_json::to_vec(&arguments).map_or(true, |bytes| bytes.len() > 64 * 1024) {
            return Err(("invalid_input", "Command arguments exceed 64 KiB"));
        }
        let ui = UiHost::interactive();
        let context =
            TurnExecutionContext::new().with_ui(ui.client("extension").expect("fixed UI owner"));
        let job = Arc::new(CommandJob {
            request_id: id,
            id: ui.snapshot().scope.to_string(),
            ui,
            context: context.clone(),
            result: Mutex::new(None),
            owner: Mutex::new(None),
            changed: Notify::new(),
        });
        state.command = Some(job.clone());
        let selection = state.selection.clone();
        let repository = self.0.repository.clone();
        let workspace = self.0.runtime.config().workspace.clone();
        let weak = Arc::downgrade(&self.0);
        let record = job.clone();
        let owner = tokio::spawn(async move {
            let _guard = Complete(record.clone());
            let result = std::panic::AssertUnwindSafe(async {
                let mut session: Option<RunSession> = match selection {
                    aicoder_core::SessionSelection::Existing(id) => Some(repository.open_run(&id)?),
                    _ => None,
                };
                anyhow::ensure!(
                    session
                        .as_ref()
                        .is_none_or(|s| s.metadata().cwd == workspace),
                    "foreign command session"
                );
                let _lease = session
                    .as_mut()
                    .map(|s| s.acquire(repository.as_ref()))
                    .transpose()?;
                extensions
                    .execute_command(
                        &name,
                        CommandRequest {
                            context: CommandContext {
                                workspace: &workspace,
                                session: session.as_ref().map(|s| s.metadata()),
                            },
                            arguments: &arguments,
                        },
                        &context,
                    )
                    .await
            })
            .catch_unwind()
            .await;
            let output = match result {
                Ok(Ok(CommandOutput::Notice(text))) => json!({"status":"completed","notice":text}),
                Ok(Ok(CommandOutput::Submit(input))) => {
                    let result = (|| {
                        let inner = weak
                            .upgrade()
                            .ok_or_else(|| anyhow::anyhow!("Host is closed"))?;
                        let host = WebHost(inner);
                        let mut state = host.0.state.lock().expect("Web state");
                        anyhow::ensure!(
                            !state.closing
                                && !context.is_cancelled()
                                && state
                                    .command
                                    .as_ref()
                                    .is_some_and(|current| Arc::ptr_eq(current, &record)),
                            "Command was superseded or cancelled"
                        );
                        anyhow::ensure!(
                            serde_json::to_vec(input.message())?.len() <= MAX_BODY,
                            "Command input exceeds the Web host bound"
                        );
                        let run = host.launch(&mut state, input);
                        record.finish(json!({"status":"submitted","run_id":run.handle.run_id()}));
                        Ok::<_, anyhow::Error>(run.handle.run_id())
                    })();
                    match result {
                        Ok(run) => json!({"status":"submitted","run_id":run}),
                        Err(_) => {
                            json!({"status":"failed","message":"Command input could not be submitted"})
                        }
                    }
                }
                _ => {
                    json!({"status":if context.is_cancelled(){"cancelled"}else{"failed"},"message":"Extension command did not complete"})
                }
            };
            record.finish(output);
        });
        *job.owner.lock().expect("command owner") = Some(owner);
        Ok(job)
    }
}

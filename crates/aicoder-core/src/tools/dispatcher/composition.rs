use super::*;
use crate::composition::*;
use crate::tools::ToolSuccess;
use futures::{StreamExt, future::BoxFuture, stream::FuturesUnordered};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use tokio::time::Instant;
struct Active {
    id: u64,
    result: Result<Message>,
}
struct ProgramOwner(Box<dyn ToolProgram>);
impl std::ops::Deref for ProgramOwner {
    type Target = dyn ToolProgram;
    fn deref(&self) -> &Self::Target {
        self.0.as_ref()
    }
}
impl std::ops::DerefMut for ProgramOwner {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.0.as_mut()
    }
}
impl Drop for ProgramOwner {
    fn drop(&mut self) {
        self.0.cancel();
    }
}
struct Children(TurnExecutionContext);
impl Drop for Children {
    fn drop(&mut self) {
        self.0.cancel("composition owner ended");
    }
}
fn collect_artifacts(content: &Value, artifacts: &mut Vec<crate::artifacts::ArtifactRef>) {
    if let Some(references) = content.get("artifacts").and_then(Value::as_array) {
        for value in references {
            if artifacts.len() < 32
                && let Ok(reference) =
                    serde_json::from_value::<crate::artifacts::ArtifactRef>(value.clone())
                && !reference.is_image()
                && !artifacts.iter().any(|old| old.id == reference.id)
            {
                artifacts.push(reference);
            }
        }
    }
}
fn image_artifact(content: &Value) -> Option<crate::artifacts::ArtifactRef> {
    let mut images = content
        .get("artifacts")?
        .as_array()?
        .iter()
        .filter_map(|value| {
            serde_json::from_value::<crate::artifacts::ArtifactRef>(value.clone()).ok()
        })
        .filter(|reference| reference.validate().is_ok() && reference.is_image());
    let image = images.next()?;
    images.next().is_none().then_some(image)
}
fn prepend_images(
    target: &mut Vec<crate::artifacts::ArtifactRef>,
    images: &BTreeMap<u64, crate::artifacts::ArtifactRef>,
) {
    let mut selected = images.values().cloned().collect::<Vec<_>>();
    for reference in std::mem::take(target) {
        if selected.len() < 32 && !selected.iter().any(|old| old.id == reference.id) {
            selected.push(reference);
        }
    }
    *target = selected;
}
impl ToolDispatcher {
    async fn composition_record(
        &self,
        journal: &dyn crate::RunJournal,
        record: CompositionRecord,
        limits: &RunLimiter,
    ) -> Result<()> {
        if let Err(error) = journal.record_composition(&record).await {
            limits.context.cancel("composition persistence failed");
            return Err(RunFailure::from_error(RunStage::Persistence, &error).into());
        }
        Ok(())
    }
    pub(super) fn execute_composition<'a>(
        &'a self,
        factory: &'a dyn ToolCompositionFactory,
        parent: &'a ModelToolCall,
        arguments: Value,
        events: Option<&'a AgentEventEmitter>,
        scope: CallScope<'a>,
    ) -> BoxFuture<'a, Result<Result<ToolSuccess, ToolFailure>>> {
        Box::pin(async move {
            let limits = scope.limits.expect("root composition scope");
            let context = scope.context.expect("root composition context");
            let journal = scope.journal.expect("root composition journal");
            let config = match factory.limits().validate() {
                Ok(config) => config,
                Err(_) => {
                    return Ok(Err(ToolFailure::new(
                        "composition_configuration",
                        "Invalid composition limits",
                    )));
                }
            };
            let allowed = factory
                .allowed_tools()
                .map(<[String]>::to_vec)
                .unwrap_or_else(|| {
                    self.registry
                        .definitions()
                        .into_iter()
                        .filter(|definition| {
                            self.registry
                                .get(&definition.name)
                                .is_some_and(|tool| tool.composition_factory().is_none())
                        })
                        .map(|definition| definition.name)
                        .collect()
                });
            if allowed.len() > 256 {
                return Ok(Err(ToolFailure::new(
                    "composition_configuration",
                    "Composition catalog exceeds 256 tools",
                )));
            }
            let mut catalog = BTreeMap::new();
            for name in &allowed {
                if name.is_empty() || name.len() > 128 || name.chars().any(char::is_control) {
                    return Ok(Err(ToolFailure::new(
                        "composition_configuration",
                        "Invalid allowlisted tool identity",
                    )));
                }
                let Some(tool) = self.registry.get(name) else {
                    return Ok(Err(ToolFailure::new(
                        "composition_configuration",
                        "Allowlisted tool is absent from the frozen registry",
                    )));
                };
                if tool.composition_factory().is_some()
                    || catalog.insert(name.clone(), tool.definition()).is_some()
                {
                    return Ok(Err(ToolFailure::new(
                        "composition_configuration",
                        "Recursive or duplicate composition tool is unavailable",
                    )));
                }
            }
            if crate::extensions::size(&catalog, 512 * 1024).is_err() {
                return Ok(Err(ToolFailure::new(
                    "composition_configuration",
                    "Composition catalog exceeds metadata budget",
                )));
            }
            if let Some(namespace) = factory.state_namespace()
                && (namespace.is_empty() || namespace.len() > 128 || !namespace.is_ascii())
            {
                return Ok(Err(ToolFailure::new(
                    "composition_configuration",
                    "Invalid public state namespace",
                )));
            }
            let saved = match factory.state_namespace() {
                Some(namespace) => journal
                    .composition_state(namespace, config.max_state_bytes)
                    .await
                    .map_err(|error| RunFailure::from_error(RunStage::Persistence, &error))?,
                None => None,
            };
            let id = uuid::Uuid::new_v4().simple().to_string();
            self.composition_record(
                journal,
                CompositionRecord::Started {
                    scope: id.clone(),
                    parent_call_id: parent.id.clone(),
                },
                limits,
            )
            .await?;
            let program = factory.start(
                CompositionContext {
                    tools: catalog.values().cloned().collect(),
                    state: saved,
                },
                arguments,
            );
            let program = match program {
                Ok(program) => program,
                Err(error) => {
                    self.composition_record(
                        journal,
                        CompositionRecord::Finished {
                            scope: id,
                            status: "start_failed".into(),
                            abandoned: Vec::new(),
                        },
                        limits,
                    )
                    .await?;
                    return Ok(Err(error));
                }
            };
            let mut program = ProgramOwner(program);
            let child = Self {
                call_hooks: self.call_hooks.clone(),
                result_hooks: self.result_hooks.clone(),
                counting: self.counting.clone(),
                hooks: self.hooks.clone(),
                registry: self.registry.clone(),
                context: self.context.clone().bounded_output(config.max_result_bytes),
                approval: self.approval.clone(),
                config: self.config.clone(),
            };
            let child_context = context.child();
            let _children = Children(child_context.clone());
            let deadline = Instant::now() + config.duration.min(self.config.tool_timeout);
            let mut pending = VecDeque::new();
            let mut seen = BTreeSet::new();
            let mut admitted = BTreeSet::new();
            let mut completed = BTreeSet::new();
            let mut running: FuturesUnordered<BoxFuture<'_, Active>> = FuturesUnordered::new();
            let mut mutation = false;
            let mut steered = false;
            let mut artifacts = Vec::new();
            let mut image_calls = BTreeSet::new();
            let mut images = BTreeMap::new();
            let driven: Result<Result<ProgramOutput, ToolFailure>> = async {
                loop {
                    limits.check()?;
                    if !steered {
                        while let Some(call) = pending.front() {
                            if mutation
                                || running.len() >= config.max_parallel.min(self.config.max_calls_per_round)
                            {
                                break;
                            }
                            let call: &ProgramCall = call;
                            let parallel = self.registry.get(&call.tool)
                                .expect("validated tool").parallel_safe();
                            if !parallel && !running.is_empty() {
                                break;
                            }
                            if context.control.as_ref().is_some_and(|control| control.has_steering()) {
                                break;
                            }
                            let call = pending.pop_front().expect("pending intent");
                            limits.admit_tool()?;
                            let tool_call = ModelToolCall {
                                id: format!("cm_{id}_{}", call.id),
                                name: call.tool,
                                arguments: call.arguments,
                            };
                            self.composition_record(journal, CompositionRecord::Call {
                                scope: id.clone(), sequence: call.id, call: tool_call.clone(),
                            }, limits).await?;
                            admitted.insert(call.id);
                            mutation = !parallel;
                            let dispatcher = &child;
                            let child_context = &child_context;
                            running.push(Box::pin(async move {
                                let result = dispatcher.execute_call(&tool_call, events, CallScope {
                                    offered_tools: None,
                                    context: Some(child_context), limits: Some(limits),
                                    journal: Some(journal), nested: true, admitted: true,
                                }).await;
                                Active { id: call.id, result }
                            }));
                            if mutation {
                                break;
                            }
                        }
                    }
                    if steered && running.is_empty() {
                        return Ok(Err(ToolFailure::new("composition_steered",
                            "Remaining program calls were interrupted by steering")));
                    }
                    enum Action {
                        Event(Result<ProgramEvent, ToolFailure>),
                        Done(Option<Active>),
                        Steer,
                        Timeout,
                        Cancel,
                    }
                    let action = tokio::select! { biased;
                        _ = context.cancelled() => Action::Cancel,
                        _ = tokio::time::sleep_until(deadline) => Action::Timeout,
                        _ = async {
                            match &context.control {
                                Some(control) => control.steering_requested().await,
                                None => std::future::pending().await,
                            }
                        }, if !steered => Action::Steer,
                        done = running.next(), if !running.is_empty() => Action::Done(done),
                        event = program.next(), if !steered => Action::Event(event),
                    };
                    match action {
                        Action::Cancel => return Err(context.error().into()),
                        Action::Timeout => return Ok(Err(ToolFailure::new("timeout",
                            "Tool program exceeded its bounded execution deadline"))),
                        Action::Steer => {
                            steered = true;
                            program.cancel();
                            pending.clear();
                        }
                        Action::Done(Some(done)) => {
                            mutation = false;
                            let message = done.result?;
                            self.composition_record(journal, CompositionRecord::Result {
                                scope: id.clone(), sequence: done.id, message: message.clone(),
                            }, limits).await?;
                            completed.insert(done.id);
                            if let Some((_, content, is_error)) = message.tool_result() {
                                collect_artifacts(content, &mut artifacts);
                                let mut content = content.clone();
                                let mut is_error = is_error;
                                if image_calls.contains(&done.id) && !is_error {
                                    if let Some(reference) = image_artifact(&content) {
                                        images.insert(done.id, reference);
                                    } else {
                                        is_error = true;
                                        content = json!({"ok":false,"error":{"code":"composition_image_result","message":"Completed image output did not return exactly one host image reference","details":{"operation_completed":true,"delivery_complete":false}}});
                                    }
                                }
                                if !steered && let Err(error) = program.respond(ProgramResponse {
                                    id: done.id, content, is_error,
                                }) {
                                    return Ok(Err(error));
                                }
                            }
                        }
                        Action::Done(None) => {}
                        Action::Event(Err(error))
                            | Action::Event(Ok(ProgramEvent::Failed(error))) => return Ok(Err(error)),
                        Action::Event(Ok(ProgramEvent::Finished(output))) => return Ok(Ok(output)),
                        Action::Event(Ok(event @ (ProgramEvent::Call(_) | ProgramEvent::ImageCall(_)))) => {
                            let (call, expose) = match event {
                                ProgramEvent::Call(call) => (call, false),
                                ProgramEvent::ImageCall(call) => (call, true),
                                _ => unreachable!(),
                            };
                            if call.id == 0 || call.id > 65536 || !seen.insert(call.id)
                                || seen.len() > config.max_calls
                            {
                                return Ok(Err(ToolFailure::new("composition_call_limit",
                                    "Program call identity/count budget exceeded")));
                            }
                            let error = if !catalog.contains_key(&call.tool) {
                                Some("tool_not_allowed")
                            } else if expose && self.registry.get(&call.tool).expect("catalog tool").capability() != ToolCapability::ReadOnly {
                                Some("image_tool_not_read_only")
                            } else if expose && image_calls.len() >= crate::tool_images::MAX_TOOL_IMAGES {
                                Some("composition_image_limit")
                            } else if crate::extensions::size(&call.arguments, config.max_request_bytes).is_err() {
                                Some("arguments_too_large")
                            } else {
                                None
                            };
                            if let Some(code) = error {
                                if let Err(error) = program.respond(ProgramResponse {
                                    id: call.id, is_error: true,
                                    content: json!({"ok":false,"error":{"code":code,"message":"Tool intent was not admitted"}}),
                                }) {
                                    return Ok(Err(error));
                                }
                            } else {
                                if expose { image_calls.insert(call.id); }
                                pending.push_back(call);
                            }
                        }
                    }
                }
            }.await;
            // End/throw/cancel never leaves an unawaited tool running under this program scope.
            child_context.cancel("tool program ended");
            program.cancel();
            let mut recording_error = None;
            while let Some(done) = running.next().await {
                if let Ok(message) = done.result {
                    if let Some((_, content, _)) = message.tool_result() {
                        collect_artifacts(content, &mut artifacts);
                    }
                    if let Err(error) = self
                        .composition_record(
                            journal,
                            CompositionRecord::Result {
                                scope: id.clone(),
                                sequence: done.id,
                                message,
                            },
                            limits,
                        )
                        .await
                    {
                        recording_error = Some(error);
                    } else {
                        completed.insert(done.id);
                    }
                }
            }
            let cleanup = tokio::time::timeout(Duration::from_secs(5), program.shutdown()).await;
            let abandoned = admitted.difference(&completed).copied().collect::<Vec<_>>();
            let finalized = async {
                if let Some(error) = recording_error {
                    return Err(error);
                }
                let driven = driven?;
                if !matches!(cleanup, Ok(Ok(()))) {
                    return Ok(Err(ToolFailure::new(
                        "composition_cleanup_incomplete",
                        "Program ended but engine cleanup is incomplete; completed tools were not retried",
                    )));
                }
                let mut output = match driven {
                    Ok(output) => output,
                    Err(mut error) => {
                        prepend_images(&mut error.artifacts, &images);
                        error.artifacts.extend(artifacts);
                        return Ok(Err(error));
                    }
                };
                limits.check()?;
                if context.control.as_ref().is_some_and(|control| control.has_steering()) {
                    let mut error = ToolFailure::new(
                        "composition_steered",
                        "Program state was not committed because steering arrived during cleanup",
                    ).with_details(output.result.output);
                    error.artifacts = output.result.artifacts;
                    prepend_images(&mut error.artifacts, &images);
                    error.artifacts.extend(artifacts);
                    return Ok(Err(error));
                }
                if let Some(value) = output.state {
                    if crate::extensions::size(&value, config.max_state_bytes).is_err() {
                        return Ok(Err(ToolFailure::new(
                            "composition_state_limit",
                            "Public state exceeds its commit budget; tool operations were not repeated",
                        )));
                    }
                    let Some(namespace) = factory.state_namespace() else {
                        return Ok(Err(ToolFailure::new(
                            "composition_state_unavailable",
                            "This host did not offer branch-local public state",
                        )));
                    };
                    if let Err(error) = journal.commit_composition_state(namespace, &value).await {
                        limits
                            .context
                            .cancel("composition state persistence failed");
                        return Err(RunFailure::from_error(RunStage::Persistence, &error).into());
                    }
                }
                output.result.output = json!({"program":output.result.output,"composition":{"scope":id,"calls":admitted.len(),"completed":completed.len(),"abandoned":abandoned}});
                prepend_images(&mut output.result.artifacts, &images);
                for reference in artifacts {
                    if output.result.artifacts.len() < 32
                        && !output
                            .result
                            .artifacts
                            .iter()
                            .any(|old| old.id == reference.id)
                    {
                        output.result.artifacts.push(reference);
                    }
                }
                Ok(Ok(output.result))
            }.await;
            let status = match &finalized {
                Ok(Ok(_)) if abandoned.is_empty() => "completed",
                Ok(Ok(_)) => "completed_with_abandoned",
                Ok(Err(_)) => "failed",
                Err(_) => "interrupted",
            };
            self.composition_record(
                journal,
                CompositionRecord::Finished {
                    scope: id,
                    status: status.into(),
                    abandoned,
                },
                limits,
            )
            .await?;
            finalized
        })
    }
}

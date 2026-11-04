use super::*;
use crate::extensions::*;
use std::future::Future;

impl ToolDispatcher {
    pub(super) async fn interception_record(
        &self,
        scope: CallScope<'_>,
        record: InterceptionRecord,
    ) -> Result<(), ToolFailure> {
        let work = timeout(self.config.tool_timeout, async {
            match scope.journal {
                Some(journal) => journal.record_interception(&record).await,
                None => Err(anyhow::anyhow!(
                    "tool interception requires an explicit supported run journal"
                )),
            }
        });
        let result=match scope.context {
            Some(context)=>tokio::select! {biased;
                _=context.cancelled()=>return Err(ToolFailure::new("extension_interception_cancelled","Interception persistence cancelled; operation completion may be uncertain")),
                result=work=>result,
            },
            None=>work.await,
        }.unwrap_or_else(|_|Err(anyhow::anyhow!("interception journal timed out")));
        result.map_err(|error| {
            if let Some(limits)=scope.limits {limits.fail_persistence(&error);}
            ToolFailure::new("extension_interception_persistence","Tool interception receipt could not be committed; inspect completed state before repeating")
        })
    }
    async fn interception_phase<T>(
        &self,
        context: Option<&TurnExecutionContext>,
        future: impl Future<Output = Result<T, ToolFailure>>,
    ) -> Result<T, ToolFailure> {
        let work = timeout(self.config.tool_timeout, future);
        let result = match context {
            Some(context) => tokio::select! {biased;
                _=context.cancelled()=>return Err(ToolFailure::new("extension_interception_cancelled","Tool interception cancelled; completed operations were not repeated")),
                _=async{match &context.control{Some(c)=>c.steering_requested().await,None=>std::future::pending().await}}=>return Err(ToolFailure::new("extension_interception_steered","Tool interception was superseded; completed operations were not repeated")),
                result=work=>result,
            },
            None => work.await,
        };
        result.unwrap_or_else(|_|Err(ToolFailure::new("extension_interception_timeout","Tool interception phase exceeded its deadline; completed operations were not repeated")))
    }
    pub(super) async fn intercept_arguments(
        &self,
        original: &ModelToolCall,
        capability: ToolCapability,
        tool_context: &ToolContext,
        scope: CallScope<'_>,
    ) -> Result<ModelToolCall, ToolFailure> {
        self.interception_phase(scope.context, async {
            crate::extensions::size(&original.arguments, MAX_INTERCEPTION_BYTES).map_err(|_| {
                ToolFailure::new(
                    "extension_interception_limit",
                    "Original tool arguments exceed the interception input budget",
                )
            })?;
            let mut call = original.clone();
            let mut bytes = 0usize;
            for hook in &self.call_hooks {
                let edit = crate::extensions::invoke(
                    &hook.owner,
                    &hook.spec,
                    tool_context.ui().clone(),
                    |cancellation| {
                        hook.callback.before_call(
                            ToolCallHookInput {
                                workspace: tool_context.workspace_root(),
                                original,
                                call: &call,
                                capability,
                            },
                            cancellation,
                        )
                    },
                )
                .await
                .map_err(|e| ToolFailure::new("extension_hook_failed", e.to_string()))?;
                match edit {
                    ToolCallEdit::Continue => {}
                    ToolCallEdit::Deny { reason } => {
                        crate::extensions::size(&reason, 4096).map_err(|_| {
                            ToolFailure::new(
                                "extension_hook_failed",
                                "Tool call denial exceeds 4 KiB",
                            )
                        })?;
                        return Err(ToolFailure::new(
                            "extension_denied",
                            crate::redaction::sanitize_text(&reason, 4096),
                        ));
                    }
                    ToolCallEdit::Arguments(arguments) => {
                        let size = crate::extensions::size(&arguments, MAX_INTERCEPTION_BYTES)
                            .map_err(|_| {
                                ToolFailure::new(
                                    "extension_interception_limit",
                                    "Transformed tool arguments exceed 64 KiB",
                                )
                            })?;
                        bytes = bytes.saturating_add(size);
                        if !arguments.is_object() || bytes > MAX_INTERCEPTION_CHAIN_BYTES {
                            return Err(ToolFailure::new(
                                "extension_interception_limit",
                                "Invalid arguments or aggregate interception budget exceeded",
                            ));
                        }
                        self.interception_record(
                            scope,
                            InterceptionRecord::Arguments {
                                call_id: call.id.clone(),
                                tool: call.name.clone(),
                                extension: hook.owner.clone(),
                                hook: hook.spec.id.clone(),
                                arguments: arguments.clone(),
                            },
                        )
                        .await?;
                        call.arguments = arguments;
                    }
                }
            }
            Ok(call)
        })
        .await
    }
    pub(super) async fn intercept_result(
        &self,
        call: &ModelToolCall,
        mut envelope: Value,
        outcome: ToolExecutionOutcome,
        operation_completed: Option<bool>,
        tool_context: &ToolContext,
        scope: CallScope<'_>,
    ) -> (Value, ToolExecutionOutcome) {
        let execution = ToolExecutionStatus::of(&outcome);
        let original_truncated = envelope["truncated"].as_bool().unwrap_or(false)
            || envelope["error"]["details"]["preview_format"] == "json_prefix";
        let original_capture_failed = !envelope["artifact_error"].is_null()
            || !envelope["error"]["details"]["artifact_error"].is_null();
        let original_artifacts: Vec<crate::artifacts::ArtifactRef> = envelope
            .get("artifacts")
            .map(|v| serde_json::from_value(v.clone()).unwrap_or_default())
            .unwrap_or_default();
        let mut changes = Vec::new();
        let transformed = self
            .interception_phase(scope.context, async {
                let mut bytes = 0usize;
                let mut metadata_bytes = 0usize;
                for hook in &self.result_hooks {
                    crate::extensions::size(&envelope, 96 * 1024).map_err(|_| {
                        ToolFailure::new(
                            "extension_interception_limit",
                            "Delivered envelope exceeds the interception input budget",
                        )
                    })?;
                    let edit = crate::extensions::invoke(
                        &hook.owner,
                        &hook.spec,
                        tool_context.ui().clone(),
                        |cancellation| {
                            hook.callback.after_tool(
                                ToolResultHookInput {
                                    workspace: tool_context.workspace_root(),
                                    call,
                                    envelope: &envelope,
                                    execution,
                                    operation_completed,
                                },
                                cancellation,
                            )
                        },
                    )
                    .await
                    .map_err(|e| ToolFailure::new("extension_hook_failed", e.to_string()))?;
                    if let Some(edit) = edit {
                        bytes = bytes.saturating_add(
                            crate::extensions::size(&edit.payload, MAX_INTERCEPTION_BYTES)
                                .map_err(|_| {
                                    ToolFailure::new(
                                        "extension_interception_limit",
                                        "Result transformation exceeds 64 KiB",
                                    )
                                })?,
                        );
                        if bytes > MAX_INTERCEPTION_CHAIN_BYTES {
                            return Err(ToolFailure::new(
                                "extension_interception_limit",
                                "Result transformations exceed aggregate budget",
                            ));
                        }
                        let is_error = edit.is_error;
                        crate::extensions::replace_result(
                            &mut envelope,
                            edit,
                            tool_context.max_output_bytes(),
                        )
                        .map_err(|_| {
                            ToolFailure::new(
                                "extension_interception_invalid",
                                "Invalid result replacement, artifact authority or output budget",
                            )
                        })?;
                        let change = InterceptionChange {
                            extension: hook.owner.clone(),
                            hook: hook.spec.id.clone(),
                            is_error,
                            artifacts: serde_json::from_value(envelope["artifacts"].clone())
                                .expect("validated references"),
                        };
                        metadata_bytes = metadata_bytes.saturating_add(
                            crate::extensions::size(&change, 4096).map_err(|_| {
                                ToolFailure::new(
                                    "extension_interception_limit",
                                    "Interception change metadata exceeds budget",
                                )
                            })?,
                        );
                        if metadata_bytes > 16 * 1024 {
                            return Err(ToolFailure::new(
                                "extension_interception_limit",
                                "Interception changes exceed aggregate metadata budget",
                            ));
                        }
                        changes.push(change);
                    }
                }
                Ok(())
            })
            .await;
        let mut delivery_complete = transformed.is_ok();
        if let Err(error) = transformed {
            envelope=failure_envelope(error.with_details(json!({"operation_completed":operation_completed,"execution_status":execution,"delivery_complete":false,"completion_uncertain":operation_completed.is_none()})));
        }
        let is_error = envelope["ok"] == false;
        let receipt = InterceptionRecord::ResultFinished {
            call_id: call.id.clone(),
            tool: call.name.clone(),
            execution,
            operation_completed,
            original_truncated,
            original_capture_failed,
            original_artifacts,
            delivered_is_error: is_error,
            delivery_complete,
            changes: changes.clone(),
        };
        if let Err(error) = self.interception_record(scope, receipt).await {
            delivery_complete = false;
            envelope=failure_envelope(error.with_details(json!({"operation_completed":operation_completed,"execution_status":execution,"delivery_complete":false})));
        }
        envelope["interception"] = json!({"execution":execution,"original_truncated":original_truncated,"original_capture_failed":original_capture_failed,"delivery_complete":delivery_complete,"changes":changes.len(),"operation_completed":operation_completed});
        let delivered = if changes.is_empty() && delivery_complete {
            outcome
        } else if envelope["ok"] == true {
            ToolExecutionOutcome::Succeeded {
                output: envelope["output"].clone(),
                truncated: envelope["truncated"] == true,
            }
        } else {
            ToolExecutionOutcome::Failed {
                code: envelope["error"]["code"]
                    .as_str()
                    .unwrap_or("extension_interception_failed")
                    .into(),
                message: envelope["error"]["message"]
                    .as_str()
                    .unwrap_or("Tool result delivery failed")
                    .into(),
            }
        };
        (envelope, delivered)
    }
}

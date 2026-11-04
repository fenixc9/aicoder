use super::*;
use crate::auxiliary::*;
impl ToolDispatcher {
    pub(super) async fn execute_auxiliary(
        &self,
        operation: &dyn AuxiliaryOperation,
        call: &ModelToolCall,
        tool_context: &ToolContext,
        scope: CallScope<'_>,
    ) -> Result<super::super::ToolSuccess, ToolFailure> {
        let (Some(limits), Some(context), Some(journal)) =
            (scope.limits, scope.context, scope.journal)
        else {
            return Err(ToolFailure::new(
                "auxiliary_scope_required",
                "Non-chat model tasks require a metered Agent run and supported journal",
            ));
        };
        operation
            .context()
            .validate(operation.model())
            .map_err(|_| {
                ToolFailure::new("auxiliary_request", "Invalid bounded classifier request")
            })?;
        let _slot = limits.auxiliary_slot().await.map_err(|_| {
            ToolFailure::new(
                "auxiliary_cancelled",
                "Auxiliary task admission was cancelled",
            )
        })?;
        let count = operation
            .context()
            .input_count(&self.counting, operation.model());
        let mut lease = limits
            .reserve_auxiliary(operation.model(), &call.id, count)
            .map_err(|_| {
                ToolFailure::new(
                    "auxiliary_budget",
                    "Auxiliary task exceeds run/model admission bounds",
                )
            })?;
        let attempt = lease.attempt();
        if let Err(error) = journal
            .record_auxiliary(&AuxiliaryRecord::Started {
                id: attempt.id,
                call_id: attempt.call_id,
                provider: attempt.provider,
                model: attempt.model,
                input_count: attempt.input_count,
            })
            .await
        {
            limits.fail_persistence(&error);
            return Err(ToolFailure::new(
                "auxiliary_persistence",
                "Auxiliary intent was not persisted; no provider was invoked",
            ));
        }
        lease.start();
        let child = context.child();
        struct Cancel(TurnExecutionContext);
        impl Drop for Cancel {
            fn drop(&mut self) {
                self.0.cancel("auxiliary task ended or dropped");
            }
        }
        let _cancel = Cancel(child.clone());
        let result = operation
            .provider()
            .classify(
                operation.model(),
                operation.context(),
                lease.reporter.clone(),
                child,
            )
            .await;
        let mut result = match result {
            Ok(answers) => match validate_answers(&answers, operation.context()) {
                Ok(()) => {
                    lease.finish("completed");
                    operation
                        .deliver(answers, lease.reporter.usage(), tool_context)
                        .await
                }
                Err(_) => {
                    lease.finish("error");
                    Err(ToolFailure::new("auxiliary_invalid_response","Classifier returned invalid answers; the completed request was not retried").with_details(json!({"operation_completed":true,"delivery_complete":false})))
                }
            },
            Err(error) => {
                lease.finish(
                    if error.kind == crate::model::ProviderErrorKind::Cancelled {
                        "interrupted"
                    } else {
                        "error"
                    },
                );
                Err(
                    ToolFailure::new("auxiliary_provider", error.to_string()).with_details(
                        json!({"operation_completed":error.kind==crate::model::ProviderErrorKind::InvalidResponse,"provider_request_uncertain":error.kind!=crate::model::ProviderErrorKind::InvalidResponse}),
                    ),
                )
            }
        };
        if let Err(error) = &mut result {
            let details = error.details.get_or_insert_with(|| json!({}));
            if let Some(usage) = lease.reporter.usage() {
                details["usage"] = json!(usage);
            }
        }
        if result.is_ok() {
            lease.delivered();
        }
        if let Err(error) = journal
            .record_auxiliary(&AuxiliaryRecord::Finished {
                attempt: lease.attempt(),
            })
            .await
        {
            limits.fail_persistence(&error);
            return Err(ToolFailure::new(
                "auxiliary_persistence",
                "Completed auxiliary request was not fully journaled; inspect state before repeating",
            ));
        }
        result
    }
}

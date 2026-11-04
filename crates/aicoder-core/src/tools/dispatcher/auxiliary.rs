use super::*;
use crate::auxiliary::*;
impl ToolDispatcher {
    pub(super) async fn execute_auxiliary(
        &self,
        plan: &dyn super::super::PreparedTool,
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
        let operation = match (
            plan.auxiliary_operation(),
            plan.image_generation_operation(),
        ) {
            (Some(op), None) => Operation::Classifier(op),
            (None, Some(op)) => Operation::Images(op),
            _ => {
                return Err(ToolFailure::new(
                    "auxiliary_request",
                    "Plan must declare exactly one native task",
                ));
            }
        };
        operation.validate().map_err(|_| {
            ToolFailure::new("auxiliary_request", "Invalid bounded native task request")
        })?;
        let _slot = limits.auxiliary_slot().await.map_err(|_| {
            ToolFailure::new(
                "auxiliary_cancelled",
                "Auxiliary task admission was cancelled",
            )
        })?;
        let count = operation.input_count(&self.counting);
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
        let result = operation.invoke(lease.reporter.clone(), child).await;
        let mut result = match result {
            Ok(answers) => match operation.validate_output(&answers) {
                Ok(()) => {
                    lease.finish("completed");
                    operation
                        .deliver(answers, lease.reporter.usage(), tool_context)
                        .await
                }
                Err(_) => {
                    lease.finish("error");
                    Err(ToolFailure::new("auxiliary_invalid_response","Native model returned invalid output; the completed request was not retried").with_details(json!({"operation_completed":true,"delivery_complete":false})))
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

enum Operation<'a> {
    Classifier(&'a dyn AuxiliaryOperation),
    Images(&'a dyn ImageGenerationOperation),
}
enum Output {
    Classifier(ClassifierAnswers),
    Images(Vec<GeneratedBlock>),
}
impl Operation<'_> {
    fn model(&self) -> &AuxiliaryModel {
        match self {
            Self::Classifier(op) => op.model(),
            Self::Images(op) => &op.model().specification,
        }
    }
    fn validate(&self) -> anyhow::Result<()> {
        match self {
            Self::Classifier(op) => op.context().validate(op.model()),
            Self::Images(op) => op.context().validate(op.model()),
        }
    }
    fn input_count(&self, counting: &crate::tokens::TokenCounting) -> crate::tokens::TokenCount {
        match self {
            Self::Classifier(op) => op.context().input_count(counting, op.model()),
            Self::Images(op) => op.context().input_count(counting, op.model()),
        }
    }
    async fn invoke(
        &self,
        usage: AuxiliaryUsageReporter,
        context: TurnExecutionContext,
    ) -> Result<Output, crate::model::ProviderError> {
        match self {
            Self::Classifier(op) => op
                .provider()
                .classify(op.model(), op.context(), usage, context)
                .await
                .map(Output::Classifier),
            Self::Images(op) => op
                .provider()
                .generate_images(op.model(), op.context(), usage, context)
                .await
                .map(Output::Images),
        }
    }
    fn validate_output(&self, output: &Output) -> anyhow::Result<()> {
        match (self, output) {
            (Self::Classifier(op), Output::Classifier(answers)) => {
                validate_answers(answers, op.context())
            }
            (Self::Images(op), Output::Images(output)) => {
                validate_generated_output(output, op.model())
            }
            _ => anyhow::bail!("native task output kind mismatch"),
        }
    }
    async fn deliver(
        &self,
        output: Output,
        usage: Option<crate::model::ModelUsage>,
        context: &ToolContext,
    ) -> Result<super::super::ToolSuccess, ToolFailure> {
        match (self, output) {
            (Self::Classifier(op), Output::Classifier(answers)) => {
                op.deliver(answers, usage, context).await
            }
            (Self::Images(op), Output::Images(output)) => op.deliver(output, usage, context).await,
            _ => Err(ToolFailure::new(
                "auxiliary_output",
                "Native task output kind mismatch",
            )),
        }
    }
}

//! Scoped tool-free model work for native boundary callbacks; routes are explicit host objects.
use super::*;
use crate::{RunJournal, TurnExecutionContext, auxiliary::*, model::*, run::RunLimiter, tokens::*};
use anyhow::{Result, ensure};
use std::sync::Arc;
pub struct BoundaryModels<'a> {
    pub(crate) owner: &'a str,
    pub(crate) hook: &'a str,
    pub(crate) limits: &'a RunLimiter,
    pub(crate) journal: &'a dyn RunJournal,
    pub(crate) counting: &'a TokenCounting,
    pub(crate) execution: &'a TurnExecutionContext,
    pub(crate) cancellation: &'a ExtensionCancellation,
}
struct Usage(AuxiliaryUsageReporter);
impl ModelEventSink for Usage {
    fn emit(&self, event: ModelEvent) {
        if let ModelEvent::Usage { usage } = event {
            self.0.record(usage);
        }
    }
}
struct Cancel(TurnExecutionContext);
impl Drop for Cancel {
    fn drop(&mut self) {
        self.0.cancel("boundary model work ended or dropped");
    }
}
impl BoundaryModels<'_> {
    /// Generate once through a host-supplied route. Known usage is captured before validation,
    /// receipt work or callback return; dropped futures retain attempted-work uncertainty.
    pub async fn generate(
        &self,
        provider: Arc<dyn ModelProvider>,
        mut request: ModelRequest,
    ) -> Result<ModelResponse> {
        ensure!(
            !self.cancellation.is_cancelled() && !self.execution.is_cancelled(),
            "boundary model scope expired"
        );
        self.limits.check()?;
        ensure!(
            request.tools.is_empty() && request.tool_choice == ModelToolChoice::None,
            "boundary model work must be tool-free"
        );
        size(&request, 2 * 1024 * 1024)?;
        let spec = provider.model_spec(&request.model)?;
        spec.validate()?;
        let output = request.options.max_output_tokens.ok_or_else(|| {
            anyhow::anyhow!("boundary work requires an explicit output reservation")
        })?;
        ensure!(
            output > 0 && output <= 8192 && spec.max_output_tokens.is_none_or(|max| output <= max),
            "invalid boundary output reservation"
        );
        let model = AuxiliaryModel {
            provider: spec.provider.clone(),
            id: spec.id.clone(),
            name: String::new(),
            images: spec.capabilities.images,
            context_tokens: spec
                .context_tokens
                .ok_or_else(|| anyhow::anyhow!("boundary model window must be known"))?,
            output_reservation: output,
        };
        model.validate()?;
        request.stream = false;
        request.messages = crate::tokens::model_messages(&request.messages, &spec).into_owned();
        provider.validate_request(&request)?;
        let count = self.counting.count(TokenInput {
            provider: &spec.provider,
            model: &spec.id,
            tokenizer: spec.tokenizer.as_deref(),
            content: TokenContent::Request {
                messages: &crate::tokens::model_messages(&request.messages, &spec),
                tools: None,
                response_format: request.options.response_format.as_ref(),
                tool_choice: Some(&request.tool_choice),
            },
        });
        let _slot = tokio::select! {biased;_=self.cancellation.cancelled()=>anyhow::bail!("boundary model scope expired"),slot=self.limits.auxiliary_slot()=>slot?};
        let mut lease = self.limits.reserve_auxiliary(
            &model,
            &format!("boundary:{}/{}", self.owner, self.hook),
            count,
        )?;
        let attempt = lease.attempt();
        self.record(&AuxiliaryRecord::Started {
            id: attempt.id,
            call_id: attempt.call_id,
            provider: attempt.provider,
            model: attempt.model,
            input_count: attempt.input_count,
        })
        .await?;
        self.limits.check()?;
        ensure!(
            !self.cancellation.is_cancelled(),
            "boundary model scope expired"
        );
        let child = self.execution.child().with_ui(self.cancellation.ui());
        let _cancel = Cancel(child.clone());
        lease.start();
        let result = tokio::select! {biased;
            _=self.cancellation.cancelled()=>Err(ProviderError::new(&spec.provider,ProviderErrorKind::Cancelled,"boundary model scope expired")),
            cancelled=child.cancelled()=>Err(ProviderError::new(&spec.provider,ProviderErrorKind::Cancelled,cancelled.to_string())),
            result=provider.generate(request,Arc::new(Usage(lease.reporter.clone())),child.clone())=>result,
        };
        if let Ok(response) = &result
            && crate::run::usage_reported(&response.usage)
        {
            lease.reporter.record(response.usage.clone());
        }
        lease.finish(if result.is_ok() { "completed" } else { "error" });
        let result = result.map_err(anyhow::Error::from).and_then(|response| {
            response.validate()?;
            ensure!(
                response.message.tool_calls().next().is_none()
                    && !response
                        .message
                        .content
                        .iter()
                        .any(|b| matches!(b, ContentBlock::Image { .. })),
                "boundary model work returned unsupported actions/media"
            );
            let bytes = size(&response, 256 * 1024)?;
            self.limits.charge_output_bytes(bytes)?;
            self.limits.check()?;
            Ok(response)
        });
        if result.is_ok() {
            lease.delivered();
        }
        self.record(&AuxiliaryRecord::Finished {
            attempt: lease.attempt(),
        })
        .await?;
        result
    }
    async fn record(&self, record: &AuxiliaryRecord) -> Result<()> {
        let result = tokio::select! {biased;
            _=self.cancellation.cancelled()=>Err(anyhow::anyhow!("boundary model scope expired")),
            cancelled=self.execution.cancelled()=>Err(cancelled.into()),
            result=tokio::time::timeout(std::time::Duration::from_secs(30),self.journal.record_auxiliary(record))=>result.map_err(|_|anyhow::anyhow!("boundary model receipt timed out")).and_then(|v|v),
        };
        if let Err(error) = &result {
            self.limits.fail_persistence(error);
        }
        result
    }
}

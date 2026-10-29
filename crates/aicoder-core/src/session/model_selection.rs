//! Read-only compatibility checks before committing a model selection or turn.
use crate::{ExecutionInput, TurnExecutionContext, TurnExecutor, context::*, model::*};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelSelectionReport {
    pub spec: ModelSpec,
    pub input_token_estimate: u64,
    pub output_reservation: u64,
    pub context_limit: Option<u64>,
    pub needs_compaction: bool,
    pub omitted_reasoning_blocks: usize,
}
impl TurnExecutor {
    /// Freeze host instructions once before any session mutation or model call.
    pub async fn prepare_input(
        &self,
        mut input: ExecutionInput,
        context: &TurnExecutionContext,
    ) -> Result<ExecutionInput> {
        if context.is_cancelled() {
            return Err(context.error().into());
        }
        input.request.stream = self.config.stream;
        let definitions = self.dispatcher.registry().definitions();
        if input.request.tools.is_empty() {
            input.request.tools = definitions;
        } else {
            ensure!(
                input
                    .request
                    .tools
                    .iter()
                    .all(|tool| definitions.contains(tool)),
                "request contains an unregistered tool definition"
            );
        }
        input.started.get_or_insert_with(tokio::time::Instant::now);
        let deadline = if let Some(duration) = self.config.budget.max_duration {
            Some(
                input
                    .started
                    .expect("preparation start")
                    .checked_add(duration)
                    .ok_or_else(|| anyhow::anyhow!("run duration is too large"))?,
            )
        } else {
            None
        };
        let started = input.started.expect("preparation start");
        let work = async {
            if input.prepared_context.is_none()
                && let Some(provider) = &self.input_context
            {
                let prepared = tokio::select! {biased;cancelled=context.cancelled()=>return Err(cancelled.into()),prepared=provider.prepare(self.workspace_root(),&input.history)=>prepared?};
                crate::input_context::validate_prefix(&prepared.system_prefix)?;
                let history = input.history.split_off(input.system_prefix_len);
                let messages = input.request.messages.split_off(input.system_prefix_len);
                input.system_prefix_len = prepared.system_prefix.len();
                input.history = prepared.system_prefix.clone();
                input.history.extend(history);
                input.request.messages = prepared.system_prefix;
                input.request.messages.extend(messages);
                input.prepared_context = Some(prepared.provenance);
            }
            Ok::<_, anyhow::Error>(())
        };
        if let Some(deadline) = deadline {
            tokio::select! {biased;
                _=tokio::time::sleep_until(deadline)=>return Err(self.preparation_budget_error(started).into()),
                result=work=>result?,
            }
        } else {
            work.await?;
        }
        self.check_preparation_budget(&input)?;
        Ok(input)
    }
    fn preparation_budget_error(
        &self,
        started: tokio::time::Instant,
    ) -> crate::run::BudgetExceeded {
        crate::run::BudgetExceeded {
            resource: crate::run::BudgetResource::Duration,
            limit: crate::run::millis(self.config.budget.max_duration.unwrap_or_default()),
            used: crate::run::millis(started.elapsed()),
            requested: None,
        }
    }
    pub(crate) fn check_preparation_budget(&self, input: &ExecutionInput) -> Result<()> {
        if let Some(started) = input.started
            && let Some(duration) = self.config.budget.max_duration
            && started.elapsed() >= duration
        {
            return Err(self.preparation_budget_error(started).into());
        }
        Ok(())
    }
    /// Validate capabilities on the projected view, preserving all canonical history.
    /// No model call, compaction generation, tool execution or journal mutation occurs.
    pub fn validate_model_selection(&self, input: &ExecutionInput) -> Result<ModelSelectionReport> {
        let spec = self.provider.model_spec(&input.request.model)?;
        spec.validate()?;
        ensure!(
            spec.id.len() <= 256
                && spec.provider.len() <= 256
                && !spec
                    .id
                    .chars()
                    .chain(spec.provider.chars())
                    .any(char::is_control),
            "invalid model selection identity"
        );
        let mut request = std::borrow::Cow::Borrowed(&input.request);
        let mut omitted = 0;
        if !spec.capabilities.reasoning_history
            && input.request.messages.iter().any(|message| {
                message.role == MessageRole::Assistant
                    && message
                        .content
                        .iter()
                        .any(|block| matches!(block, ContentBlock::Reasoning { .. }))
            })
        {
            for message in &mut request.to_mut().messages {
                if message.role == MessageRole::Assistant {
                    message.content.retain(|block| {
                        let keep = !matches!(block, ContentBlock::Reasoning { .. });
                        if !keep {
                            omitted += 1;
                        }
                        keep
                    });
                }
            }
            request
                .to_mut()
                .messages
                .retain(|message| !message.content.is_empty());
        }
        let estimate = estimate_context_tokens(&request.messages, Some(&request.tools)) as u64;
        let output = u64::from(
            request
                .options
                .max_output_tokens
                .or(spec.max_output_tokens)
                .unwrap_or(0),
        );
        if request.messages.is_empty() {
            request.to_mut().messages.push(Message::text(
                MessageRole::User,
                "[Empty-branch compatibility check]",
            ));
        }
        self.provider.validate_request(&request)?;
        let compaction_input = ContextCompactionInput {
            model: &input.request.model,
            messages: &input.request.messages,
            tools: Some(&input.request.tools),
            max_output_tokens: input.request.options.max_output_tokens,
        };
        let needs_compaction = self.context_compactor.as_ref().is_some_and(|compactor| {
            compactor.estimate_tokens(compaction_input) > compactor.target_tokens(compaction_input)
        });
        if let Some(window) = spec.context_tokens {
            ensure!(
                output < window,
                "output reservation leaves no model context"
            );
            if let Some(compactor) = &self.context_compactor {
                ensure!(
                    (compactor.target_tokens(compaction_input) as u64).saturating_add(output)
                        <= window,
                    "context policy exceeds the selected model window; rebuild the host policy"
                );
            }
            ensure!(
                estimate.saturating_add(output) <= window || needs_compaction,
                "projected history exceeds the selected model window and no compaction is configured"
            );
        }
        Ok(ModelSelectionReport {
            context_limit: spec.context_tokens,
            spec,
            input_token_estimate: estimate,
            output_reservation: output,
            needs_compaction,
            omitted_reasoning_blocks: omitted,
        })
    }
}

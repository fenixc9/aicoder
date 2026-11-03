//! Read-only compatibility checks before committing a model selection or turn.
use crate::{ExecutionInput, TurnExecutionContext, TurnExecutor, context::*, model::*};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelSelectionReport {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<ReasoningEffort>,
    pub spec: ModelSpec,
    pub input_token_estimate: u64,
    pub output_reservation: u64,
    pub context_limit: Option<u64>,
    pub needs_compaction: bool,
    pub omitted_reasoning_blocks: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_count: Option<crate::tokens::TokenCount>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_decision: Option<crate::context_policy::ContextDecision>,
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
        self.prepare_model_tools(&mut input.request)?;
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
                let prepared = tokio::select! {biased;cancelled=context.cancelled()=>return Err(cancelled.into()),prepared=provider.prepare_for_execution(self.workspace_root(),&input.history,input.system_prefix_len,context)=>prepared?};
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
    pub(crate) fn decide_context(
        &self,
        request: &ModelRequest,
        spec: &ModelSpec,
        count: &crate::tokens::TokenCount,
    ) -> Result<crate::context_policy::ContextDecision> {
        self.decide_context_view(request, &request.messages, spec, count)
    }
    pub(crate) fn decide_context_view(
        &self,
        request: &ModelRequest,
        messages: &[Message],
        spec: &ModelSpec,
        count: &crate::tokens::TokenCount,
    ) -> Result<crate::context_policy::ContextDecision> {
        let input = ContextCompactionInput {
            model: &request.model,
            messages,
            tools: Some(&request.tools),
            max_output_tokens: request.options.max_output_tokens,
        };
        crate::context_policy::decide(
            self.context_policy.as_ref(),
            &crate::context_policy::ContextPolicyInput {
                spec,
                messages,
                tools: &request.tools,
                options: &request.options,
                tool_choice: &request.tool_choice,
                count,
                compactor_target: self
                    .context_compactor
                    .as_ref()
                    .map(|compactor| compactor.target_tokens(input) as u64),
                legacy_compactor_estimate: self
                    .context_compactor
                    .as_ref()
                    .filter(|compactor| !compactor.supports_counting())
                    .map(|compactor| compactor.estimate_tokens(input) as u64),
                output_reservation: u64::from(
                    request
                        .options
                        .max_output_tokens
                        .or(spec.max_output_tokens)
                        .unwrap_or(0),
                ),
            },
        )
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
        let definitions = self.model_tool_definitions();
        if request.tools.is_empty() && !definitions.is_empty() {
            request.to_mut().tools = definitions;
        } else {
            ensure!(
                request.tools.iter().all(|tool| definitions.contains(tool)),
                "request tools must match the host model catalog"
            );
        }
        let omitted = if spec.capabilities.reasoning_history {
            0
        } else {
            input
                .request
                .messages
                .iter()
                .filter(|message| message.role == MessageRole::Assistant)
                .flat_map(|message| &message.content)
                .filter(|block| matches!(block, ContentBlock::Reasoning { .. }))
                .count()
        };
        if let std::borrow::Cow::Owned(projected) =
            crate::tokens::model_messages(&input.request.messages, &spec)
        {
            request.to_mut().messages = projected;
        }
        let measured = self.counting.request(&request, &spec);
        let estimate = measured.tokens;
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
        let decision = self.decide_context(&request, &spec, &measured)?;
        let needs_compaction = matches!(
            decision,
            crate::context_policy::ContextDecision::Compact { .. }
        );
        if let crate::context_policy::ContextDecision::Reject { code } = &decision {
            anyhow::bail!("context policy rejected model selection: {code}");
        }
        Ok(ModelSelectionReport {
            reasoning_effort: request.options.reasoning_effort,
            context_limit: spec.context_tokens,
            spec,
            input_token_estimate: estimate,
            output_reservation: output,
            needs_compaction,
            omitted_reasoning_blocks: omitted,
            token_count: Some(measured),
            context_decision: Some(decision),
        })
    }
}

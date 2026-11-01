//! Structured outcomes, retained progress and resource accounting shared by every host.
use crate::{
    TurnExecutionContext,
    model::{Message, ModelUsage, ProviderError, StopReason},
    redaction::sanitize_text,
};
use serde::{Deserialize, Serialize};
use std::{error::Error, fmt, ops::Deref, sync::Mutex, time::Duration};
use tokio::time::Instant;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct RunBudget {
    pub max_tool_calls: Option<usize>,
    pub max_tokens: Option<u64>,
    pub max_duration: Option<Duration>,
    /// Cumulative payload bytes before transport escaping; diagnostic records are excluded.
    pub max_output_bytes: Option<usize>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BudgetResource {
    Rounds,
    ToolCalls,
    ToolCallsPerRound,
    Tokens,
    Duration,
    OutputBytes,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BudgetExceeded {
    pub resource: BudgetResource,
    pub limit: u64,
    pub used: u64,
    pub requested: Option<u64>,
}
impl fmt::Display for BudgetResource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Rounds => "model rounds",
            Self::ToolCalls => "tool calls",
            Self::ToolCallsPerRound => "tool calls per round",
            Self::Tokens => "tokens",
            Self::Duration => "execution time (ms)",
            Self::OutputBytes => "output bytes",
        })
    }
}
impl fmt::Display for BudgetExceeded {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.resource == BudgetResource::Rounds {
            return write!(
                f,
                "Turn execution exceeded maximum of {} model rounds",
                self.limit
            );
        }
        write!(
            f,
            "run stopped at {} budget (used {}, limit {}",
            self.resource, self.used, self.limit
        )?;
        if let Some(requested) = self.requested {
            write!(f, ", next {requested}")?;
        }
        f.write_str(")")
    }
}
impl Error for BudgetExceeded {}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStage {
    #[default]
    Preparing,
    Compaction,
    Model,
    Tools,
    Verification,
    Persistence,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunFailure {
    pub stage: RunStage,
    pub message: String,
    pub provider: Option<ProviderError>,
}
impl RunFailure {
    pub fn from_error(stage: RunStage, error: &anyhow::Error) -> Self {
        Self {
            stage,
            message: sanitize_text(&format!("{error:#}"), 4096),
            provider: error.downcast_ref::<ProviderError>().cloned(),
        }
    }
}
impl fmt::Display for RunFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}
impl Error for RunFailure {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.provider.as_ref().map(|p| p as _)
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "detail", rename_all = "snake_case")]
pub enum RunStop {
    Cancelled { reason: String },
    Budget(BudgetExceeded),
    Model(StopReason),
    Failed(RunFailure),
}
impl fmt::Display for RunStop {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cancelled { reason } => f.write_str(reason),
            Self::Budget(error) => error.fmt(f),
            Self::Model(reason) => write!(
                f,
                "model stopped without completing: {}",
                model_stop_text(reason)
            ),
            Self::Failed(error) => error.fmt(f),
        }
    }
}
impl RunStop {
    pub(crate) fn error(&self) -> anyhow::Error {
        match self {
            Self::Cancelled { reason } => crate::TurnCancelled::new(reason.clone()).into(),
            Self::Budget(error) => error.clone().into(),
            Self::Failed(error) => error.clone().into(),
            Self::Model(reason) => ModelStopped(reason.clone()).into(),
        }
    }
}
fn model_stop_text(reason: &StopReason) -> String {
    match reason {
        StopReason::Complete => "complete".into(),
        StopReason::ToolCalls => "tool calls".into(),
        StopReason::MaxTokens => "output token limit".into(),
        StopReason::Filtered => "filtered response".into(),
        StopReason::Other(value) => sanitize_text(value, 256),
    }
}
#[derive(Debug, Clone)]
pub struct ModelStopped(pub StopReason);
impl fmt::Display for ModelStopped {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "model stopped without completing: {}",
            model_stop_text(&self.0)
        )
    }
}
impl Error for ModelStopped {}
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenAccounting {
    pub used: u64,
    pub estimated: bool,
}
impl TokenAccounting {
    pub fn from_usage(usage: &ModelUsage, estimate: u64) -> Self {
        let reported = usage.total_tokens.or_else(|| {
            usage
                .input_tokens
                .zip(usage.output_tokens)
                .and_then(|(input, output)| input.checked_add(output))
        });
        let floor = usage
            .input_tokens
            .unwrap_or(0)
            .saturating_add(usage.output_tokens.unwrap_or(0));
        Self {
            used: reported.map(|reported| reported.max(floor)).unwrap_or(
                estimate.max(
                    usage
                        .input_tokens
                        .unwrap_or(0)
                        .saturating_add(usage.output_tokens.unwrap_or(0)),
                ),
            ),
            estimated: reported.is_none() || reported.is_some_and(|reported| reported < floor),
        }
    }
}
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartialToolCall {
    pub index: u32,
    pub id: String,
    pub name: String,
    pub arguments: String,
}
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartialModelResponse {
    pub text: String,
    pub reasoning: String,
    pub tool_calls: Vec<PartialToolCall>,
}
impl PartialModelResponse {
    pub fn is_empty(&self) -> bool {
        self.text.is_empty() && self.reasoning.is_empty() && self.tool_calls.is_empty()
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunProgress {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_context: Option<crate::ModelContextInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_context: Option<serde_json::Value>,
    #[serde(default)]
    pub event_delivery: crate::events::EventDeliveryReport,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending_inputs: Vec<crate::RunInput>,
    pub messages: Vec<Message>,
    pub input_messages: usize,
    pub usage: ModelUsage,
    pub reported_usage: bool,
    pub rounds: usize,
    pub tool_calls: usize,
    pub output_bytes: usize,
    pub tokens: TokenAccounting,
    pub elapsed: Duration,
    pub stage: RunStage,
    pub last_model_stop: Option<StopReason>,
    pub partial_response: Option<PartialModelResponse>,
}
impl RunProgress {
    pub fn new(messages: Vec<Message>) -> Self {
        Self {
            input_context: None,
            model_context: None,
            event_delivery: crate::events::EventDeliveryReport::default(),
            pending_inputs: Vec::new(),
            input_messages: messages.len(),
            messages,
            usage: ModelUsage::zero(),
            reported_usage: false,
            rounds: 0,
            tool_calls: 0,
            output_bytes: 0,
            tokens: TokenAccounting::default(),
            elapsed: Duration::ZERO,
            stage: RunStage::Preparing,
            last_model_stop: None,
            partial_response: None,
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnExecutionResult {
    pub final_message: Message,
    pub finish_reason: Option<StopReason>,
    pub progress: RunProgress,
}
impl Deref for TurnExecutionResult {
    type Target = RunProgress;
    fn deref(&self) -> &RunProgress {
        &self.progress
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InterruptedTurn {
    #[serde(default)]
    pub persistence_error: Option<Box<RunFailure>>,
    pub stop: RunStop,
    pub progress: RunProgress,
}
impl Deref for InterruptedTurn {
    type Target = RunProgress;
    fn deref(&self) -> &RunProgress {
        &self.progress
    }
}
impl fmt::Display for InterruptedTurn {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.stop.fmt(f)
    }
}
impl InterruptedTurn {
    pub fn into_error(self) -> anyhow::Error {
        let mut error = self.stop.error();
        if let Some(failure) = &self.persistence_error {
            error = error.context(failure.clone());
        }
        error.context(self)
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", content = "result", rename_all = "snake_case")]
pub enum RunOutcome {
    Completed(TurnExecutionResult),
    Interrupted(InterruptedTurn),
}
impl RunOutcome {
    pub(crate) fn progress_mut(&mut self) -> &mut RunProgress {
        match self {
            Self::Completed(result) => &mut result.progress,
            Self::Interrupted(turn) => &mut turn.progress,
        }
    }
    pub fn progress(&self) -> &RunProgress {
        match self {
            Self::Completed(r) => &r.progress,
            Self::Interrupted(t) => &t.progress,
        }
    }
    pub fn stop(&self) -> Option<&RunStop> {
        match self {
            Self::Completed(_) => None,
            Self::Interrupted(t) => Some(&t.stop),
        }
    }
    pub fn into_result(self) -> anyhow::Result<TurnExecutionResult> {
        match self {
            Self::Completed(r) => Ok(r),
            Self::Interrupted(t) => Err(t.into_error()),
        }
    }
}
#[derive(Default)]
struct Usage {
    output_bytes: usize,
    output_limited: bool,
    tool_calls: usize,
    tokens: TokenAccounting,
    stop: Option<BudgetExceeded>,
}
pub(crate) struct RunLimiter {
    pub budget: RunBudget,
    pub context: TurnExecutionContext,
    pub started: Instant,
    state: Mutex<Usage>,
}
impl RunLimiter {
    pub fn new(budget: RunBudget, context: TurnExecutionContext) -> Self {
        Self {
            budget,
            context,
            started: Instant::now(),
            state: Mutex::new(Usage::default()),
        }
    }
    pub fn stop(&self, violation: BudgetExceeded) {
        let mut state = self.state.lock().expect("run budget poisoned");
        if state.stop.is_none() && !self.context.is_cancelled() {
            state.stop = Some(violation);
            drop(state);
            self.context.cancel("run budget reached");
        }
    }
    pub fn violation(&self) -> Option<BudgetExceeded> {
        self.state.lock().expect("run budget poisoned").stop.clone()
    }
    pub fn check(&self) -> anyhow::Result<()> {
        if self.budget.max_output_bytes == Some(0) {
            self.stop(BudgetExceeded {
                resource: BudgetResource::OutputBytes,
                limit: 0,
                used: 0,
                requested: None,
            });
        }
        if let Some(limit) = self.budget.max_tokens {
            let used = self.state.lock().expect("run budget poisoned").tokens.used;
            if used > limit {
                self.stop(BudgetExceeded {
                    resource: BudgetResource::Tokens,
                    limit,
                    used,
                    requested: None,
                });
            }
        }
        if let Some(duration) = self.budget.max_duration
            && self.started.elapsed() >= duration
        {
            self.stop(BudgetExceeded {
                resource: BudgetResource::Duration,
                limit: millis(duration),
                used: millis(self.started.elapsed()),
                requested: None,
            });
        }
        if let Some(stop) = self.violation() {
            return Err(stop.into());
        }
        if self.context.is_cancelled() {
            return Err(self.context.error().into());
        }
        Ok(())
    }
    pub fn retain_text(&self, text: &str) -> String {
        let mut state = self.state.lock().expect("run budget poisoned");
        if state.output_limited {
            return String::new();
        }
        let available = self
            .budget
            .max_output_bytes
            .map(|n| n.saturating_sub(state.output_bytes))
            .unwrap_or(usize::MAX);
        let mut end = text.len().min(available);
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        state.output_bytes = state.output_bytes.saturating_add(end);
        state.output_limited |= end < text.len();
        let violation = (end < text.len()).then(|| BudgetExceeded {
            resource: BudgetResource::OutputBytes,
            limit: self.budget.max_output_bytes.unwrap() as u64,
            used: state.output_bytes as u64,
            requested: Some(text.len() as u64),
        });
        drop(state);
        if let Some(violation) = violation {
            self.stop(violation);
        }
        text[..end].to_string()
    }
    pub fn output_limited(&self) -> bool {
        self.state
            .lock()
            .expect("run budget poisoned")
            .output_limited
    }
    pub fn reserve_tools(&self, count: usize, per_round: usize) -> anyhow::Result<()> {
        self.check()?;
        let used = self.state.lock().expect("run budget poisoned").tool_calls;
        let violation = if count > per_round {
            Some(BudgetExceeded {
                resource: BudgetResource::ToolCallsPerRound,
                limit: per_round as u64,
                used: 0,
                requested: Some(count as u64),
            })
        } else {
            self.budget
                .max_tool_calls
                .filter(|limit| count > limit.saturating_sub(used))
                .map(|limit| BudgetExceeded {
                    resource: BudgetResource::ToolCalls,
                    limit: limit as u64,
                    used: used as u64,
                    requested: Some(count as u64),
                })
        };
        if let Some(stop) = violation {
            self.stop(stop.clone());
            return Err(stop.into());
        }
        Ok(())
    }
    pub fn admit_tool(&self) -> anyhow::Result<()> {
        self.check()?;
        let mut state = self.state.lock().expect("run budget poisoned");
        if let Some(limit) = self.budget.max_tool_calls
            && state.tool_calls >= limit
        {
            let stop = BudgetExceeded {
                resource: BudgetResource::ToolCalls,
                limit: limit as u64,
                used: state.tool_calls as u64,
                requested: Some(1),
            };
            drop(state);
            self.stop(stop.clone());
            return Err(stop.into());
        }
        state.tool_calls = state.tool_calls.saturating_add(1);
        Ok(())
    }
    pub fn remaining_tokens(&self) -> Option<u64> {
        self.budget.max_tokens.map(|limit| {
            limit.saturating_sub(self.state.lock().expect("run budget poisoned").tokens.used)
        })
    }
    pub fn account_tokens(&self, usage: &ModelUsage, estimate: u64) {
        self.account_compaction(&TokenAccounting::from_usage(usage, estimate));
    }
    pub fn account_compaction(&self, accounting: &TokenAccounting) {
        let mut state = self.state.lock().expect("run budget poisoned");
        state.tokens.used = state.tokens.used.saturating_add(accounting.used);
        state.tokens.estimated |= accounting.estimated;
    }
    pub fn snapshot(&self, progress: &mut RunProgress) {
        let state = self.state.lock().expect("run budget poisoned");
        progress.tool_calls = state.tool_calls;
        progress.output_bytes = state.output_bytes;
        progress.tokens = state.tokens.clone();
        progress.elapsed = self.started.elapsed();
    }
}
pub(crate) fn millis(duration: Duration) -> u64 {
    duration.as_millis().try_into().unwrap_or(u64::MAX)
}
pub(crate) fn usage_reported(usage: &ModelUsage) -> bool {
    usage.input_tokens.is_some()
        || usage.output_tokens.is_some()
        || usage.total_tokens.is_some()
        || usage.cached_input_tokens.is_some()
        || usage.cache_write_tokens.is_some()
}

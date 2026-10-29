//! Host context decisions over measured provider projections; policies never rewrite history.
use crate::{model::*, tokens::*};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum ContextDecision {
    Keep,
    Compact { target_tokens: u64 },
    Reject { code: String },
}
pub struct ContextPolicyInput<'a> {
    pub spec: &'a ModelSpec,
    pub messages: &'a [Message],
    pub tools: &'a [ToolDefinition],
    pub options: &'a ModelOptions,
    pub tool_choice: &'a ModelToolChoice,
    pub count: &'a TokenCount,
    pub compactor_target: Option<u64>,
    /// Compatibility with compactors whose old estimate function is an independent policy.
    pub legacy_compactor_estimate: Option<u64>,
    pub output_reservation: u64,
}
pub trait ContextPolicy: Send + Sync {
    fn name(&self) -> &'static str;
    fn decide(&self, input: &ContextPolicyInput<'_>) -> Result<ContextDecision>;
}
#[derive(Default)]
pub struct WindowContextPolicy;
impl ContextPolicy for WindowContextPolicy {
    fn name(&self) -> &'static str {
        "window_v1"
    }
    fn decide(&self, input: &ContextPolicyInput<'_>) -> Result<ContextDecision> {
        let known = input
            .spec
            .context_tokens
            .map(|window| window.saturating_sub(input.output_reservation));
        if known == Some(0) {
            return Ok(ContextDecision::Reject {
                code: "no_input_capacity".into(),
            });
        }
        if let (Some(target), Some(known)) = (input.compactor_target, known)
            && target > known
        {
            return Ok(ContextDecision::Reject {
                code: "configured_context_exceeds_model".into(),
            });
        }
        let target = input.compactor_target.or(known);
        let count = input
            .count
            .tokens
            .max(input.legacy_compactor_estimate.unwrap_or(0));
        Ok(match target {
            Some(target) if count > target => {
                if input.compactor_target.is_some() {
                    ContextDecision::Compact {
                        target_tokens: target,
                    }
                } else {
                    ContextDecision::Reject {
                        code: "model_context_exceeded".into(),
                    }
                }
            }
            _ => ContextDecision::Keep,
        })
    }
}
pub(crate) fn decide(
    policy: &dyn ContextPolicy,
    input: &ContextPolicyInput<'_>,
) -> Result<ContextDecision> {
    ensure!(
        !policy.name().is_empty()
            && policy.name().len() <= 128
            && policy.name().is_ascii()
            && !policy.name().chars().any(char::is_control),
        "invalid context policy name"
    );
    let decision = policy.decide(input)?;
    let known = input
        .spec
        .context_tokens
        .map(|window| window.saturating_sub(input.output_reservation));
    match &decision {
        ContextDecision::Keep => ensure!(
            known.is_none_or(|target| input.count.tokens <= target),
            "context policy cannot keep input beyond a known model window"
        ),
        ContextDecision::Compact { target_tokens } => {
            ensure!(
                *target_tokens > 0 && known.is_none_or(|known| *target_tokens <= known),
                "invalid context policy target"
            );
            ensure!(
                input.compactor_target.is_some(),
                "context policy requested compaction without a compactor"
            );
        }
        ContextDecision::Reject { code } => ensure!(
            !code.is_empty()
                && code.len() <= 128
                && code.is_ascii()
                && !code.chars().any(char::is_control),
            "invalid context policy rejection"
        ),
    }
    Ok(decision)
}

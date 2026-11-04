//! Typed non-chat model tasks. Host routes and credentials never come from task input/history.
use crate::{
    TurnExecutionContext,
    model::*,
    tokens::*,
    tools::{ToolContext, ToolFailure, ToolSuccess},
};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};
pub const AUXILIARY_NAMESPACE: &str = "aicoder.auxiliary.v1";
pub const MAX_AUXILIARY_CALLS: usize = 128;
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClassifierModel {
    pub provider: String,
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub images: bool,
    #[serde(default = "default_context")]
    pub context_tokens: u64,
    #[serde(default = "default_reservation")]
    pub output_reservation: u32,
}
fn default_context() -> u64 {
    65536
}
fn default_reservation() -> u32 {
    1024
}
impl ClassifierModel {
    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            [&self.provider, &self.id]
                .iter()
                .all(|s| !s.is_empty() && s.len() <= 128 && !s.chars().any(char::is_control))
                && self.name.len() <= 256
                && (1..=1024 * 1024).contains(&self.context_tokens)
                && self.output_reservation <= 8192,
            "invalid classifier specification"
        );
        Ok(())
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ClassifierQuestion {
    Choice {
        instructions: String,
        criteria: BTreeMap<String, String>,
    },
    Score {
        instructions: String,
        criteria: Vec<String>,
    },
    Bool {
        instructions: String,
        criteria: BooleanCriteria,
    },
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BooleanCriteria {
    #[serde(rename = "true")]
    pub positive: String,
    #[serde(rename = "false")]
    pub negative: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClassifierContext {
    pub state: BTreeMap<String, Value>,
    pub questions: BTreeMap<String, ClassifierQuestion>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<ImageSource>,
}
fn text(value: &str) -> bool {
    !value.is_empty() && value.len() <= 4096
}
fn key(value: &str) -> bool {
    !value.is_empty() && value.len() <= 64 && !value.chars().any(char::is_control)
}
impl ClassifierContext {
    pub fn validate(&self, model: &ClassifierModel) -> anyhow::Result<()> {
        model.validate()?;
        anyhow::ensure!(
            !self.questions.is_empty()
                && self.questions.len() <= 32
                && self.images.len() <= 4
                && (self.images.is_empty() || model.images),
            "invalid classifier questions/image capability"
        );
        for (id, q) in &self.questions {
            anyhow::ensure!(key(id), "invalid classifier question identity");
            let valid = match q {
                ClassifierQuestion::Choice {
                    instructions,
                    criteria,
                } => {
                    text(instructions)
                        && (2..=32).contains(&criteria.len())
                        && criteria.iter().all(|(k, v)| key(k) && text(v))
                }
                ClassifierQuestion::Score {
                    instructions,
                    criteria,
                } => {
                    text(instructions)
                        && (2..=32).contains(&criteria.len())
                        && criteria.iter().all(|s| text(s))
                }
                ClassifierQuestion::Bool {
                    instructions,
                    criteria,
                } => text(instructions) && text(&criteria.positive) && text(&criteria.negative),
            };
            anyhow::ensure!(valid, "invalid classifier criteria");
        }
        for image in &self.images {
            match image {
                ImageSource::Base64 { media_type, data } => anyhow::ensure!(
                    crate::media::image_media_type(media_type).is_some()
                        && crate::agent_input::inline_base64(data)
                        && data.len() <= 2 * 1024 * 1024,
                    "invalid classifier image"
                ),
                ImageSource::Artifact { reference } => anyhow::ensure!(
                    reference.validate().is_ok() && reference.is_image(),
                    "invalid classifier image reference"
                ),
                ImageSource::Url { .. } => {
                    anyhow::bail!("classifier tasks do not fetch image URLs")
                }
            }
        }
        #[derive(Serialize)]
        struct Metadata<'a> {
            state: &'a BTreeMap<String, Value>,
            questions: &'a BTreeMap<String, ClassifierQuestion>,
        }
        crate::extensions::size(
            &Metadata {
                state: &self.state,
                questions: &self.questions,
            },
            64 * 1024,
        )?;
        Ok(())
    }
    pub fn input_count(&self, counting: &TokenCounting, model: &ClassifierModel) -> TokenCount {
        let metadata = serde_json::json!({"state":self.state,"questions":self.questions});
        let mut count = counting.count(TokenInput {
            provider: &model.provider,
            model: &model.id,
            tokenizer: None,
            content: TokenContent::Text(&metadata.to_string()),
        });
        count.tokens = count
            .tokens
            .saturating_add(64)
            .saturating_add((self.images.len() as u64) * 8192);
        count.scope = TokenScope::Request;
        count.accuracy = TokenAccuracy::Estimated;
        count.fallback = Some(TokenFallback::UnsupportedContent);
        count.method = "auxiliary_json_v1".into();
        count
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ClassifierAnswer {
    Choice {
        choice: String,
        probabilities: BTreeMap<String, f64>,
        confidence: f64,
    },
    Score {
        score: f64,
        confidence: f64,
    },
    Bool {
        probability: f64,
    },
}
pub type ClassifierAnswers = BTreeMap<String, ClassifierAnswer>;
pub fn validate_answers(
    answers: &ClassifierAnswers,
    context: &ClassifierContext,
) -> anyhow::Result<()> {
    let probability = |p: f64| p.is_finite() && (0.0..=1.0).contains(&p);
    anyhow::ensure!(
        answers.len() == context.questions.len(),
        "classifier answer set mismatch"
    );
    for (id, q) in &context.questions {
        let valid = match (q, answers.get(id)) {
            (ClassifierQuestion::Bool { .. }, Some(ClassifierAnswer::Bool { probability: p })) => {
                probability(*p)
            }
            (
                ClassifierQuestion::Score { criteria, .. },
                Some(ClassifierAnswer::Score { score, confidence }),
            ) => {
                score.is_finite()
                    && *score >= 0.0
                    && *score <= (criteria.len() - 1) as f64
                    && probability(*confidence)
            }
            (
                ClassifierQuestion::Choice { criteria, .. },
                Some(ClassifierAnswer::Choice {
                    choice,
                    probabilities,
                    confidence,
                }),
            ) => {
                criteria.contains_key(choice)
                    && probabilities.len() == criteria.len()
                    && probabilities
                        .iter()
                        .all(|(key, value)| criteria.contains_key(key) && probability(*value))
                    && (probabilities.values().sum::<f64>() - 1.0).abs() <= 0.001
                    && probability(*confidence)
            }
            _ => false,
        };
        anyhow::ensure!(valid, "invalid classifier answer");
    }
    Ok(())
}
#[derive(Clone, Default)]
pub struct AuxiliaryUsageReporter(Arc<Mutex<Option<ModelUsage>>>);
impl AuxiliaryUsageReporter {
    /// A cumulative measurement for this single task attempt, recorded before output validation.
    pub fn record(&self, usage: ModelUsage) {
        *self.0.lock().expect("auxiliary usage poisoned") = Some(usage);
    }
    pub fn usage(&self) -> Option<ModelUsage> {
        self.0.lock().expect("auxiliary usage poisoned").clone()
    }
}
#[async_trait]
pub trait ClassifierProvider: Send + Sync {
    async fn classify(
        &self,
        model: &ClassifierModel,
        context: &ClassifierContext,
        usage: AuxiliaryUsageReporter,
        cancellation: TurnExecutionContext,
    ) -> Result<ClassifierAnswers, ProviderError>;
}
/// Immutable prepared host operation. Core owns its sole invocation after normal approval.
#[async_trait]
pub trait AuxiliaryOperation: Send + Sync {
    fn model(&self) -> &ClassifierModel;
    fn context(&self) -> &ClassifierContext;
    fn provider(&self) -> &dyn ClassifierProvider;
    async fn deliver(
        &self,
        answers: ClassifierAnswers,
        usage: Option<ModelUsage>,
        context: &ToolContext,
    ) -> Result<ToolSuccess, ToolFailure>;
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuxiliaryAttempt {
    pub id: String,
    pub call_id: String,
    pub provider: String,
    pub model: String,
    pub status: String,
    pub usage: ModelUsage,
    pub reported_usage: bool,
    pub tokens: crate::run::TokenAccounting,
    pub input_count: TokenCount,
    pub delivery_complete: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AuxiliaryRecord {
    Started {
        id: String,
        call_id: String,
        provider: String,
        model: String,
        input_count: TokenCount,
    },
    Finished {
        attempt: AuxiliaryAttempt,
    },
}

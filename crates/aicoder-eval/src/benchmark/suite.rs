use super::*;
use aicoder_core::{ModelProvider, TurnExecutionContext, model::*};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex},
};
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CodingSuite {
    pub schema_version: u32,
    pub id: String,
    pub cases: Vec<CodingCase>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CodingCase {
    pub id: String,
    pub prompt: String,
    pub files: BTreeMap<String, String>,
    pub allowed_changes: Vec<String>,
    pub grading: Grading,
    pub script: Vec<ModelToolCall>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Grading {
    RustTests { tests: String },
    Json { path: String, expected: Value },
}
impl CodingSuite {
    pub fn builtin() -> Result<Self> {
        let suite: Self = serde_json::from_str(include_str!("../../fixtures/coding-v1.json"))?;
        suite.validate()?;
        Ok(suite)
    }
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let suite: Self =
            serde_json::from_slice(&crate::bounds::read(path.as_ref(), 1024 * 1024)?)?;
        suite.validate()?;
        Ok(suite)
    }
    pub fn fingerprint(&self) -> Result<String> {
        Ok(crate::bounds::sha256(&crate::bounds::encode(
            self,
            1024 * 1024,
        )?))
    }
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.schema_version == 1
                && valid_id(&self.id)
                && !self.cases.is_empty()
                && self.cases.len() <= 32,
            "invalid coding suite identity/count/version"
        );
        crate::bounds::encoded_size(self, 1024 * 1024)?;
        let mut ids = BTreeSet::new();
        for case in &self.cases {
            ensure!(
                valid_id(&case.id) && ids.insert(&case.id),
                "invalid or duplicate coding case id"
            );
            ensure!(
                !case.prompt.is_empty()
                    && case.prompt.len() <= 16384
                    && !case.files.is_empty()
                    && case.files.len() <= 64
                    && case.allowed_changes.len() <= 64
                    && case.script.len() <= 16,
                "coding case input budget exceeded"
            );
            for (name, text) in &case.files {
                relative(name)?;
                for parent in Path::new(name).ancestors().skip(1) {
                    ensure!(
                        !case
                            .files
                            .contains_key(&parent.to_string_lossy().to_string()),
                        "fixture file/directory conflict"
                    );
                }
                ensure!(text.len() <= 65536, "fixture file budget exceeded");
            }
            for name in &case.allowed_changes {
                relative(name)?;
            }
            match &case.grading {
                Grading::RustTests { tests } => ensure!(
                    !tests.is_empty() && tests.len() <= 65536,
                    "Rust grader budget exceeded"
                ),
                Grading::Json { path, expected } => {
                    relative(path)?;
                    crate::bounds::encoded_size(expected, 65536)?;
                }
            }
        }
        Ok(())
    }
}
pub(super) fn valid_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"_-".contains(&c))
}
pub(super) fn relative(value: &str) -> Result<()> {
    ensure!(
        !value.is_empty()
            && !value.chars().any(char::is_control)
            && value
                .split('/')
                .all(|part| !part.is_empty() && part != "." && part != "..")
            && value.len() <= 256
            && !value.contains('\\')
            && Path::new(value)
                .components()
                .all(|c| matches!(c, std::path::Component::Normal(_))),
        "invalid relative benchmark path"
    );
    ensure!(
        !Path::new(value)
            .components()
            .any(|c| matches!(c.as_os_str().to_str(), Some(".git" | "target" | ".env"))),
        "reserved benchmark fixture path"
    );
    Ok(())
}
impl CodingCase {
    pub fn request(
        &self,
        model: &str,
        options: ModelOptions,
        system: Option<&str>,
    ) -> ModelRequest {
        let mut messages = Vec::new();
        if let Some(system) = system {
            messages.push(Message::text(MessageRole::System, system));
        }
        messages.push(Message::text(MessageRole::User, &self.prompt));
        let mut request = ModelRequest::new(model, messages);
        request.options = options;
        request.stream = false;
        request
    }
    pub(crate) fn materialize(&self, path: &Path) -> Result<()> {
        for (name, text) in &self.files {
            let path = path.join(name);
            std::fs::create_dir_all(path.parent().expect("relative fixture"))?;
            std::fs::write(path, text)?;
        }
        Ok(())
    }
}
/// Fixed in-memory model responses are contract fixtures, never a coding-success benchmark.
pub struct ScriptedCodingProvider {
    script: Vec<ModelToolCall>,
    position: Mutex<usize>,
    variant: ScriptVariant,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum ScriptVariant {
    #[default]
    Correct,
    Noop,
    ProviderError,
    Limit,
}
impl ScriptedCodingProvider {
    pub fn new(case: &CodingCase, variant: ScriptVariant) -> Self {
        Self {
            script: case.script.clone(),
            position: Mutex::new(0),
            variant,
        }
    }
}
#[async_trait]
impl ModelProvider for ScriptedCodingProvider {
    fn model_spec(&self, model: &str) -> std::result::Result<ModelSpec, ProviderError> {
        let mut spec = ModelSpec::new(
            format!("scripted-coding-{:?}", self.variant).to_lowercase(),
            model,
        );
        spec.context_tokens = Some(65536);
        spec.capabilities.tools = true;
        spec.capabilities.seed = true;
        spec.capabilities.temperature = true;
        Ok(spec)
    }
    async fn generate(
        &self,
        request: ModelRequest,
        sink: Arc<dyn ModelEventSink>,
        _: TurnExecutionContext,
    ) -> std::result::Result<ModelResponse, ProviderError> {
        if self.variant == ScriptVariant::ProviderError {
            return Err(ProviderError::new(
                "scripted-coding",
                ProviderErrorKind::Other,
                "deliberate contract provider failure",
            ));
        }
        let mut position = self.position.lock().expect("script position poisoned");
        let call = match self.variant {
            ScriptVariant::Correct => self.script.get(*position).cloned(),
            ScriptVariant::Limit => Some(ModelToolCall {
                id: format!("loop-{}", *position),
                name: "missing_tool".into(),
                arguments: json!({}),
            }),
            _ => None,
        };
        *position += 1;
        let response = ModelResponse {
            id: None,
            model: request.model,
            message: match &call {
                Some(call) => Message {
                    role: MessageRole::Assistant,
                    content: vec![ContentBlock::ToolCall { call: call.clone() }],
                },
                None => Message::text(MessageRole::Assistant, "Coding fixture completed"),
            },
            stop_reason: if call.is_some() {
                StopReason::ToolCalls
            } else {
                StopReason::Complete
            },
            usage: ModelUsage {
                input_tokens: Some(4),
                output_tokens: Some(1),
                total_tokens: Some(5),
                ..Default::default()
            },
        };
        emit_response(&response, sink.as_ref());
        Ok(response)
    }
}
pub(crate) struct CodingGrader {
    pub case: CodingCase,
    pub excluded: Vec<String>,
}
#[async_trait]
impl crate::Evaluator for CodingGrader {
    fn name(&self) -> &str {
        "hidden_coding_tests"
    }
    async fn evaluate(
        &self,
        context: &crate::EvaluationContext<'_>,
    ) -> Result<crate::EvaluationResult> {
        use crate::{EvalFinding, EvalSeverity, EvaluationResult};
        match &self.case.grading {
            Grading::Json { path, expected } => {
                let actual = context
                    .workspace_after
                    .file_content(path)
                    .and_then(|b| serde_json::from_slice::<Value>(b).ok());
                if actual.as_ref() == Some(expected) {
                    Ok(EvaluationResult::passed(self.name()))
                } else {
                    Ok(EvaluationResult::failed(
                        self.name(),
                        vec![EvalFinding::new(
                            EvalSeverity::Error,
                            "json_mismatch",
                            "Candidate JSON differs from the private semantic expectation",
                        )],
                    ))
                }
            }
            Grading::RustTests { tests } => {
                // The grading workspace and hidden source do not exist until the Agent has ended.
                let grader = tempfile::tempdir()?;
                std::fs::write(
                    grader.path().join("rust-toolchain.toml"),
                    include_str!("../../../../rust-toolchain.toml"),
                )?;
                for path in context
                    .workspace_after
                    .paths()
                    .filter(|p| p.extension().is_some_and(|e| e == "rs"))
                {
                    let Some(bytes) = context.workspace_after.file_content(path) else {
                        continue;
                    };
                    let target = grader.path().join("candidate").join(path);
                    std::fs::create_dir_all(target.parent().expect("candidate path"))?;
                    std::fs::write(target, bytes)?;
                }
                std::fs::write(grader.path().join("grader.rs"), tests)?;
                let built = crate::command::capture(
                    std::ffi::OsStr::new("rustc"),
                    &[
                        "--edition=2024".into(),
                        "--test".into(),
                        "grader.rs".into(),
                        "-o".into(),
                        "grader".into(),
                    ],
                    grader.path(),
                    std::time::Duration::from_secs(30),
                    16384,
                    &self.excluded,
                )
                .await?;
                if built.exit_code != Some(0)
                    || !built.cleanup_ok
                    || !built.reaped
                    || !built.output_complete
                {
                    return Ok(EvaluationResult::failed(
                        self.name(),
                        vec![
                            EvalFinding::new(
                                EvalSeverity::Error,
                                "candidate_compile_failed",
                                "Candidate failed trusted Rust compilation",
                            )
                            .with_evidence(built.stderr),
                        ],
                    ));
                }
                let tested = crate::command::capture(
                    grader.path().join("grader").as_os_str(),
                    &["--test-threads=1".into()],
                    grader.path(),
                    std::time::Duration::from_secs(5),
                    16384,
                    &self.excluded,
                )
                .await?;
                if tested.exit_code == Some(0)
                    && tested.cleanup_ok
                    && tested.reaped
                    && tested.output_complete
                {
                    Ok(EvaluationResult::passed(self.name()))
                } else {
                    Ok(EvaluationResult::failed(
                        self.name(),
                        vec![
                            EvalFinding::new(
                                EvalSeverity::Error,
                                "hidden_tests_failed",
                                "Candidate failed hidden Rust behavior tests",
                            )
                            .with_evidence(format!("{}\n{}", tested.stdout, tested.stderr)),
                        ],
                    ))
                }
            }
        }
    }
}

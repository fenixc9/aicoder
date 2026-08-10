use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

use aicoder_core::types::{ChatCompletionRequest, ChatMessage, Role};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

use crate::{EvalCase, EvalReport, WorkspaceFixture};

/// One row from an official SWE-bench family dataset.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SweBenchInstance {
    pub repo: String,
    pub instance_id: String,
    pub base_commit: String,
    #[serde(default)]
    pub patch: String,
    #[serde(default)]
    pub test_patch: String,
    pub problem_statement: String,
    #[serde(default)]
    pub hints_text: String,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub version: String,
    #[serde(
        rename = "FAIL_TO_PASS",
        default,
        deserialize_with = "deserialize_test_list"
    )]
    pub fail_to_pass: Vec<String>,
    #[serde(
        rename = "PASS_TO_PASS",
        default,
        deserialize_with = "deserialize_test_list"
    )]
    pub pass_to_pass: Vec<String>,
    #[serde(default)]
    pub environment_setup_commit: String,
    #[serde(default)]
    pub issue_id: Option<Value>,
    #[serde(default)]
    pub issue_url: Option<String>,
    #[serde(default)]
    pub pr_url: Option<String>,
    #[serde(default)]
    pub difficulty: Option<String>,
}

/// Locally loaded SWE-bench dataset. Both JSON arrays and JSONL exports are supported.
#[derive(Debug, Clone)]
pub struct SweBenchDataset {
    pub name: String,
    pub instances: Vec<SweBenchInstance>,
}

impl SweBenchDataset {
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let content = fs::read_to_string(path)
            .with_context(|| format!("Failed to read SWE-bench dataset {}", path.display()))?;
        let instances = parse_instances(&content)
            .with_context(|| format!("Failed to parse SWE-bench dataset {}", path.display()))?;
        validate_instances(&instances)?;
        let name = path
            .file_stem()
            .and_then(|value| value.to_str())
            .unwrap_or("swe-bench")
            .to_string();
        Ok(Self { name, instances })
    }

    pub fn from_instances(
        name: impl Into<String>,
        instances: Vec<SweBenchInstance>,
    ) -> Result<Self> {
        validate_instances(&instances)?;
        Ok(Self {
            name: name.into(),
            instances,
        })
    }

    pub fn len(&self) -> usize {
        self.instances.len()
    }

    pub fn is_empty(&self) -> bool {
        self.instances.is_empty()
    }

    pub fn get(&self, instance_id: &str) -> Option<&SweBenchInstance> {
        self.instances
            .iter()
            .find(|instance| instance.instance_id == instance_id)
    }

    pub fn filtered(&self, filter: &SweBenchFilter) -> Result<Self> {
        let instance_ids = filter.instance_ids.iter().collect::<HashSet<_>>();
        let repositories = filter.repositories.iter().collect::<HashSet<_>>();
        let mut instances = self
            .instances
            .iter()
            .filter(|instance| {
                (instance_ids.is_empty() || instance_ids.contains(&instance.instance_id))
                    && (repositories.is_empty() || repositories.contains(&instance.repo))
                    && filter.difficulty.as_ref().is_none_or(|difficulty| {
                        instance
                            .difficulty
                            .as_deref()
                            .is_some_and(|value| value.eq_ignore_ascii_case(difficulty))
                    })
            })
            .cloned()
            .collect::<Vec<_>>();
        if let Some(limit) = filter.limit {
            instances.truncate(limit);
        }
        ensure!(
            !instances.is_empty(),
            "SWE-bench filter selected no instances"
        );
        Self::from_instances(self.name.clone(), instances)
    }
}

#[derive(Debug, Clone, Default)]
pub struct SweBenchFilter {
    pub instance_ids: Vec<String>,
    pub repositories: Vec<String>,
    pub difficulty: Option<String>,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone)]
pub enum SweBenchRepositorySource {
    GitHub,
    LocalRoot(PathBuf),
}

#[derive(Debug, Clone)]
pub struct SweBenchAdapter {
    model: String,
    model_name_or_path: String,
    repository_source: SweBenchRepositorySource,
    include_hints: bool,
    system_prompt: String,
    temperature: Option<f32>,
    max_tokens: Option<i32>,
}

impl SweBenchAdapter {
    pub fn new(model: impl Into<String>) -> Self {
        let model = model.into();
        Self {
            model_name_or_path: model.clone(),
            model,
            repository_source: SweBenchRepositorySource::GitHub,
            include_hints: true,
            system_prompt: concat!(
                "You are solving a SWE-bench software engineering task. ",
                "The repository root is the current working directory; use relative paths and do not assume /repo exists. ",
                "Diagnose the issue, implement a minimal fix, ",
                "and run relevant tests. Do not only describe a patch."
            )
            .to_string(),
            temperature: Some(0.0),
            max_tokens: Some(4096),
        }
    }

    pub fn model_name_or_path(mut self, value: impl Into<String>) -> Self {
        self.model_name_or_path = value.into();
        self
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    pub fn model_name(&self) -> &str {
        &self.model_name_or_path
    }

    pub fn repository_source(mut self, source: SweBenchRepositorySource) -> Self {
        self.repository_source = source;
        self
    }

    pub fn include_hints(mut self, include: bool) -> Self {
        self.include_hints = include;
        self
    }

    pub fn system_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.system_prompt = prompt.into();
        self
    }

    pub fn temperature(mut self, temperature: Option<f32>) -> Self {
        self.temperature = temperature;
        self
    }

    pub fn max_tokens(mut self, max_tokens: Option<i32>) -> Self {
        self.max_tokens = max_tokens;
        self
    }

    pub fn adapt(&self, instance: &SweBenchInstance) -> Result<SweBenchCase> {
        validate_repo_name(&instance.repo)?;
        let repository = self.repository_location(&instance.repo);
        let request = ChatCompletionRequest {
            model: self.model.clone(),
            messages: vec![
                chat_message(Role::System, self.system_prompt.clone()),
                chat_message(Role::User, self.user_prompt(instance)),
            ],
            temperature: self.temperature,
            top_p: Some(1.0),
            max_tokens: self.max_tokens,
            seed: None,
            tools: None,
            tool_choice: None,
            stream: None,
            stream_options: None,
            stop: None,
            response_format: None,
        };
        Ok(SweBenchCase {
            instance: instance.clone(),
            eval_case: EvalCase::new(
                instance.instance_id.clone(),
                request,
                WorkspaceFixture::git_checkout(repository, instance.base_commit.clone()),
            ),
        })
    }

    pub fn adapt_dataset(&self, dataset: &SweBenchDataset) -> Result<Vec<SweBenchCase>> {
        dataset
            .instances
            .iter()
            .map(|instance| self.adapt(instance))
            .collect()
    }

    pub fn prediction(
        &self,
        case: &SweBenchCase,
        report: &EvalReport,
    ) -> Result<SweBenchPrediction> {
        ensure!(
            report.case.id == case.instance.instance_id,
            "Evaluation report {} does not belong to SWE-bench instance {}",
            report.case.id,
            case.instance.instance_id
        );
        Ok(SweBenchPrediction {
            instance_id: case.instance.instance_id.clone(),
            model_patch: report.workspace_patch.clone().unwrap_or_default(),
            model_name_or_path: self.model_name_or_path.clone(),
        })
    }

    fn user_prompt(&self, instance: &SweBenchInstance) -> String {
        let mut prompt = instance.problem_statement.trim().to_string();
        if self.include_hints && !instance.hints_text.trim().is_empty() {
            prompt.push_str("\n\nHints:\n");
            prompt.push_str(instance.hints_text.trim());
        }
        prompt
    }

    fn repository_location(&self, repo: &str) -> String {
        match &self.repository_source {
            SweBenchRepositorySource::GitHub => format!("https://github.com/{repo}.git"),
            SweBenchRepositorySource::LocalRoot(root) => {
                let nested = repo
                    .split_once('/')
                    .map(|(owner, name)| root.join(owner).join(name))
                    .expect("validated SWE-bench repository name");
                let flattened = root.join(repo.replace('/', "__"));
                if nested.exists() || !flattened.exists() {
                    nested.to_string_lossy().into_owned()
                } else {
                    flattened.to_string_lossy().into_owned()
                }
            }
        }
    }
}

/// Persistent bare-repository cache used by batch evaluations.
#[derive(Debug, Clone)]
pub struct SweBenchRepositoryCache {
    root: PathBuf,
}

impl SweBenchRepositoryCache {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn prepare(&self, instances: &[SweBenchInstance]) -> Result<()> {
        fs::create_dir_all(&self.root).with_context(|| {
            format!(
                "Failed to create SWE-bench repository cache {}",
                self.root.display()
            )
        })?;
        let mut repositories = HashSet::new();
        for instance in instances {
            if repositories.insert(instance.repo.as_str()) {
                self.prepare_repository(&instance.repo)?;
            }
        }
        for instance in instances {
            self.ensure_commit(&instance.repo, &instance.base_commit)?;
        }
        Ok(())
    }

    fn prepare_repository(&self, repo: &str) -> Result<()> {
        validate_repo_name(repo)?;
        let cached = self.path_for(repo);
        if cached.exists() {
            ensure!(
                cached.is_dir(),
                "Repository cache is not a directory: {}",
                cached.display()
            );
            return Ok(());
        }
        let temporary = cached.with_extension(format!("clone-{}.tmp", std::process::id()));
        let output = Command::new("git")
            .args(["init", "--quiet", "--bare", "--"])
            .arg(&temporary)
            .output()
            .with_context(|| format!("Failed to initialize repository cache for {repo}"))?;
        ensure_git_success("git init --bare", output)?;
        let output = Command::new("git")
            .arg("-C")
            .arg(&temporary)
            .args(["remote", "add", "origin"])
            .arg(format!("https://github.com/{repo}.git"))
            .output()
            .with_context(|| format!("Failed to configure repository cache for {repo}"))?;
        ensure_git_success("git remote add", output)?;
        fs::rename(&temporary, &cached)
            .with_context(|| format!("Failed to publish repository cache {}", cached.display()))
    }

    fn ensure_commit(&self, repo: &str, commit: &str) -> Result<()> {
        ensure_safe_commit(commit)?;
        let cached = self.path_for(repo);
        let object = format!("{commit}^{{commit}}");
        let present = Command::new("git")
            .arg("-C")
            .arg(&cached)
            .args(["cat-file", "-e"])
            .arg(&object)
            .output()
            .with_context(|| format!("Failed to inspect cached commit {commit} for {repo}"))?;
        let reference = format!("refs/heads/aicoder-cache-{commit}");
        if present.status.success() {
            let output = Command::new("git")
                .arg("-C")
                .arg(&cached)
                .args(["update-ref", &reference, commit])
                .output()
                .with_context(|| {
                    format!("Failed to reference cached commit {commit} for {repo}")
                })?;
            ensure_git_success("git update-ref", output)?;
            return Ok(());
        }
        let refspec = format!("{commit}:{reference}");
        let output = Command::new("git")
            .arg("-C")
            .arg(&cached)
            .args(["fetch", "--quiet", "--depth=1", "origin"])
            .arg(refspec)
            .output()
            .with_context(|| format!("Failed to fetch cached commit {commit} for {repo}"))?;
        ensure_git_success("git fetch", output)?;
        Ok(())
    }

    fn path_for(&self, repo: &str) -> PathBuf {
        self.root.join(repo.replace('/', "__"))
    }
}

#[derive(Debug, Clone)]
pub struct SweBenchCase {
    pub instance: SweBenchInstance,
    pub eval_case: EvalCase,
}

/// Prediction record accepted by the official SWE-bench evaluation harness.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SweBenchPrediction {
    pub instance_id: String,
    pub model_patch: String,
    pub model_name_or_path: String,
}

pub fn write_swebench_predictions(
    path: impl AsRef<Path>,
    predictions: &[SweBenchPrediction],
) -> Result<()> {
    let path = path.as_ref();
    let encoded = serde_json::to_string_pretty(predictions)
        .context("Failed to encode SWE-bench predictions")?;
    fs::write(path, encoded)
        .with_context(|| format!("Failed to write SWE-bench predictions {}", path.display()))
}

fn chat_message(role: Role, content: String) -> ChatMessage {
    ChatMessage {
        role,
        content: Some(content),
        reasoning: None,
        tool_calls: None,
        tool_call_id: None,
        name: None,
    }
}

fn parse_instances(content: &str) -> Result<Vec<SweBenchInstance>> {
    let content = content.trim();
    ensure!(!content.is_empty(), "SWE-bench dataset is empty");
    if let Ok(instances) = serde_json::from_str::<Vec<SweBenchInstance>>(content) {
        return Ok(instances);
    }
    if let Ok(instance) = serde_json::from_str::<SweBenchInstance>(content) {
        return Ok(vec![instance]);
    }
    content
        .lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .map(|(index, line)| {
            serde_json::from_str::<SweBenchInstance>(line)
                .with_context(|| format!("Invalid SWE-bench JSONL record at line {}", index + 1))
        })
        .collect()
}

fn validate_instances(instances: &[SweBenchInstance]) -> Result<()> {
    ensure!(
        !instances.is_empty(),
        "SWE-bench dataset contains no instances"
    );
    let mut ids = HashSet::new();
    for instance in instances {
        ensure!(
            !instance.instance_id.trim().is_empty(),
            "SWE-bench instance id cannot be empty"
        );
        ensure!(
            ids.insert(instance.instance_id.as_str()),
            "Duplicate SWE-bench instance id: {}",
            instance.instance_id
        );
        validate_repo_name(&instance.repo)?;
        ensure!(
            !instance.base_commit.is_empty(),
            "SWE-bench instance {} has no base commit",
            instance.instance_id
        );
    }
    Ok(())
}

fn validate_repo_name(repo: &str) -> Result<()> {
    let Some((owner, name)) = repo.split_once('/') else {
        anyhow::bail!("Invalid SWE-bench repository name: {repo}");
    };
    let valid_component = |component: &str| {
        !component.is_empty()
            && component != "."
            && component != ".."
            && component
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    };
    ensure!(
        !name.contains('/') && valid_component(owner) && valid_component(name),
        "Invalid SWE-bench repository name: {repo}"
    );
    Ok(())
}

fn ensure_safe_commit(commit: &str) -> Result<()> {
    ensure!(
        (7..=64).contains(&commit.len()) && commit.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "Invalid Git base commit: {commit}"
    );
    Ok(())
}

fn ensure_git_success(operation: &str, output: Output) -> Result<()> {
    if output.status.success() {
        Ok(())
    } else {
        anyhow::bail!(
            "{operation} failed with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        )
    }
}

fn deserialize_test_list<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Value::deserialize(deserializer)?;
    match value {
        Value::Null => Ok(Vec::new()),
        Value::Array(values) => values
            .into_iter()
            .map(|value| match value {
                Value::String(value) => Ok(value),
                other => Err(serde::de::Error::custom(format!(
                    "test list contains non-string value: {other}"
                ))),
            })
            .collect(),
        Value::String(value) if value.trim().is_empty() => Ok(Vec::new()),
        Value::String(value) => {
            serde_json::from_str::<Vec<String>>(&value).or_else(|_| Ok(vec![value]))
        }
        other => Err(serde::de::Error::custom(format!(
            "test list must be a JSON string or array, got {other}"
        ))),
    }
}

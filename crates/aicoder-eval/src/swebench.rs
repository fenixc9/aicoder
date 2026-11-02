use aicoder_core::model::{ModelOptions, ModelToolChoice};
use std::{
    collections::{BTreeMap, HashSet},
    fs,
    path::{Path, PathBuf},
};

use aicoder_core::model::{Message, MessageRole, ModelRequest};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

use crate::{EvalCase, EvalReport, WorkspaceFixture};

/// One row from an official SWE-bench family dataset.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
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
    /// Preserve current grading/image/test metadata for identity, never include it in prompts.
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
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
        let content = String::from_utf8(crate::bounds::read(path, 256 * 1024 * 1024)?)?;
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
        ensure!(
            instance_ids.len() == filter.instance_ids.len(),
            "duplicate requested SWE-bench instance id"
        );
        ensure!(
            instance_ids.iter().all(|id| self.get(id).is_some()),
            "requested SWE-bench instance is absent from the dataset"
        );
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
            !instances.is_empty() && instances.len() <= 20_000,
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
    options: ModelOptions,
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
            options:ModelOptions {max_output_tokens:Some(4096),..Default::default()},
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
        self.options.temperature = temperature;
        self
    }

    pub fn max_tokens(mut self, max_tokens: Option<u32>) -> Self {
        self.options.max_output_tokens = max_tokens;
        self
    }

    pub fn model_options(mut self, options: ModelOptions) -> Self {
        self.options = options;
        self
    }
    pub fn prepend_system_prompt(mut self, prompt: &str) -> Self {
        if !prompt.is_empty() {
            self.system_prompt = format!("{prompt}\n\n{}", self.system_prompt);
        }
        self
    }

    pub fn adapt(&self, instance: &SweBenchInstance) -> Result<SweBenchCase> {
        validate_repo_name(&instance.repo)?;
        let repository = self.repository_location(&instance.repo);
        let request = ModelRequest {
            model: self.model.clone(),
            messages: vec![
                chat_message(MessageRole::System, self.system_prompt.clone()),
                chat_message(MessageRole::User, self.user_prompt(instance)),
            ],
            options: self.options.clone(),
            tools: Vec::new(),
            tool_choice: ModelToolChoice::Auto,
            stream: false,
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
        ensure!(
            report.case.model == case.eval_case.request.model
                && report.workspace_capture_complete
                && report.workspace_patch.is_some(),
            "generation patch is unavailable or belongs to a different model"
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
        let cache = self.clone();
        let rows = instances.to_vec();
        std::thread::spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?
                .block_on(cache.prepare_with_context(
                    &rows,
                    aicoder_core::TurnExecutionContext::new(),
                    &[],
                ))
        })
        .join()
        .map_err(|_| anyhow::anyhow!("repository cache owner panicked"))?
    }
    pub async fn prepare_with_context(
        &self,
        instances: &[SweBenchInstance],
        context: aicoder_core::TurnExecutionContext,
        excluded: &[String],
    ) -> Result<()> {
        fs::create_dir_all(&self.root)?;
        let storage = crate::storage::StorageRoot::existing(&self.root)?;
        let _lease = storage.lease()?;
        let mut repositories = HashSet::new();
        for instance in instances {
            validate_repo_name(&instance.repo)?;
            ensure_safe_commit(&instance.base_commit)?;
            if repositories.insert(&instance.repo) {
                self.prepare_repository(&instance.repo, context.clone(), excluded)
                    .await?;
            }
        }
        for instance in instances {
            self.ensure_commit(
                &instance.repo,
                &instance.base_commit,
                context.clone(),
                excluded,
            )
            .await?;
        }
        Ok(())
    }
    async fn prepare_repository(
        &self,
        repo: &str,
        context: aicoder_core::TurnExecutionContext,
        excluded: &[String],
    ) -> Result<()> {
        let cached = self.path_for(repo);
        if cached.exists() {
            ensure!(
                fs::symlink_metadata(&cached)?.is_dir(),
                "repository cache must be an owned directory"
            );
            return Ok(());
        }
        let temporary = tempfile::tempdir_in(&self.root)?;
        crate::git::run(
            temporary.path(),
            vec!["init".into(), "--quiet".into(), "--bare".into()],
            None,
            context.clone(),
            excluded,
        )
        .await?;
        crate::git::run(
            temporary.path(),
            vec![
                "remote".into(),
                "add".into(),
                "origin".into(),
                format!("https://github.com/{repo}.git").into(),
            ],
            None,
            context,
            excluded,
        )
        .await?;
        fs::rename(temporary.path(), &cached)?;
        Ok(())
    }
    async fn ensure_commit(
        &self,
        repo: &str,
        commit: &str,
        context: aicoder_core::TurnExecutionContext,
        excluded: &[String],
    ) -> Result<()> {
        let cached = self.path_for(repo);
        let object = format!("{commit}^{{commit}}");
        let present = crate::git::run(
            &cached,
            vec!["cat-file".into(), "-e".into(), object.into()],
            None,
            context.clone(),
            excluded,
        )
        .await
        .is_ok();
        ensure!(
            !context.is_cancelled(),
            "repository cache preparation cancelled"
        );
        let reference = format!("refs/heads/aicoder-cache-{commit}");
        if present {
            crate::git::run(
                &cached,
                vec!["update-ref".into(), reference.into(), commit.into()],
                None,
                context,
                excluded,
            )
            .await?;
        } else {
            crate::git::run(
                &cached,
                vec![
                    "fetch".into(),
                    "--quiet".into(),
                    "--depth=1".into(),
                    "origin".into(),
                    format!("{commit}:{reference}").into(),
                ],
                None,
                context,
                excluded,
            )
            .await?;
        }
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
    ensure!(
        predictions.len() <= 5000,
        "prediction count budget exceeded"
    );
    let mut ids = HashSet::new();
    for prediction in predictions {
        ensure!(
            valid_instance_id(&prediction.instance_id)
                && valid_model_label(&prediction.model_name_or_path)
                && ids.insert(&prediction.instance_id)
                && prediction.model_patch.len() <= 8 * 1024 * 1024,
            "invalid prediction identity/patch budget"
        );
    }
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    let mut writer = crate::bounds::LimitedWriter {
        inner: temporary.as_file_mut(),
        remaining: 256 * 1024 * 1024,
    };
    serde_json::to_writer(&mut writer, predictions)?;
    use std::io::Write;
    writer.flush()?;
    temporary.as_file().sync_all()?;
    temporary.persist(path).map_err(|e| e.error)?;
    Ok(())
}

fn chat_message(role: MessageRole, content: String) -> Message {
    Message::text(role, content)
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
            valid_instance_id(&instance.instance_id),
            "SWE-bench instance id cannot be empty"
        );
        ensure!(
            ids.insert(instance.instance_id.as_str()),
            "Duplicate SWE-bench instance id: {}",
            instance.instance_id
        );
        validate_repo_name(&instance.repo)?;
        crate::bounds::encoded_size(instance, 4 * 1024 * 1024)?;
        ensure!(
            !instance.problem_statement.trim().is_empty(),
            "SWE-bench issue text is empty"
        );
        ensure_safe_commit(&instance.base_commit)?;
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
                _ => Err(serde::de::Error::custom(
                    "test list contains a non-string value",
                )),
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

pub(crate) fn valid_instance_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id != "."
        && id != ".."
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
}
pub(crate) fn valid_run_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
}
impl SweBenchInstance {
    pub fn fingerprint(&self) -> Result<String> {
        Ok(crate::bounds::sha256(&crate::bounds::encode(
            self,
            4 * 1024 * 1024,
        )?))
    }
}
impl SweBenchCase {
    pub fn input_fingerprint(&self) -> Result<String> {
        Ok(crate::bounds::sha256(&crate::bounds::encode(
            &serde_json::json!({"instance":self.instance.fingerprint()?,"request":self.eval_case.request,"fixture":self.eval_case.fixture}),
            8 * 1024 * 1024,
        )?))
    }
}

pub(crate) fn valid_model_label(label: &str) -> bool {
    !label.is_empty()
        && label.len() <= 128
        && label != "."
        && label != ".."
        && label.replace('/', "__").len() <= 128
        && label
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_./:-".contains(&b))
}

//! Copyable native-policy example. It protects installed tools, not arbitrary host Rust/IO.
use aicoder_core::{extensions::*, model::*, tools::*};
use anyhow::{Result, ensure};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs::{File, OpenOptions},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Component, Path, PathBuf},
    sync::Arc,
};
use tokio::sync::RwLock;

struct Binding {
    path: PathBuf,
    file: Option<File>,
}
struct Mode {
    planning: bool,
    epoch: u64,
}
pub struct CodingSafety {
    workspace: PathBuf,
    prefixes: Vec<PathBuf>,
    bindings: Vec<Binding>,
    mode: Arc<RwLock<Mode>>,
}
fn denied(code: &str, message: &str) -> ToolFailure {
    ToolFailure::new(code, message)
}
impl CodingSafety {
    pub fn new(workspace: &Path, prefixes: &[&str]) -> Result<Arc<Self>> {
        let workspace = workspace.canonicalize()?;
        ensure!(
            !prefixes.is_empty() && prefixes.len() <= 64,
            "provide 1..64 protected prefixes"
        );
        ensure!(
            prefixes.iter().map(|p| p.len()).sum::<usize>() <= 8192,
            "protected-prefix byte budget exceeded"
        );
        let mut paths = Vec::new();
        let mut names = BTreeMap::new();
        names.insert(workspace.clone(), ());
        let mut components = 0;
        for prefix in prefixes {
            let path = Path::new(prefix);
            ensure!(
                !path.as_os_str().is_empty()
                    && path.components().all(|c| matches!(c, Component::Normal(_))),
                "protected prefixes must be relative without traversal"
            );
            let mut absolute = workspace.clone();
            for part in path.components() {
                components += 1;
                ensure!(
                    components <= 128,
                    "protected-prefix component budget exceeded"
                );
                absolute.push(part.as_os_str());
                names.insert(absolute.clone(), ());
            }
            paths.push(path.to_path_buf());
        }
        let mut bindings = Vec::new();
        for path in names.into_keys() {
            let file = match std::fs::symlink_metadata(&path) {
                Ok(meta) => {
                    ensure!(
                        !meta.file_type().is_symlink() && (meta.is_dir() || meta.is_file()),
                        "protected configuration cannot contain links or special files"
                    );
                    let file = OpenOptions::new()
                        .read(true)
                        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
                        .open(&path)?;
                    ensure!(
                        file.metadata()?.dev() == meta.dev()
                            && file.metadata()?.ino() == meta.ino(),
                        "protected route changed during construction"
                    );
                    Some(file)
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(error) => return Err(error.into()),
            };
            bindings.push(Binding { path, file });
        }
        Ok(Arc::new(Self {
            workspace,
            prefixes: paths,
            bindings,
            mode: Arc::new(RwLock::new(Mode {
                planning: false,
                epoch: 0,
            })),
        }))
    }
    fn workspace(&self, workspace: &Path) -> Result<(), ToolFailure> {
        if workspace != self.workspace {
            return Err(denied(
                "policy_workspace",
                "Policy is bound to another workspace",
            ));
        }
        Ok(())
    }
    fn bindings(&self) -> Result<(), ToolFailure> {
        for binding in &self.bindings {
            let current = std::fs::symlink_metadata(&binding.path);
            let unchanged = match (&binding.file, current) {
                (None, Err(error)) if error.kind() == std::io::ErrorKind::NotFound => true,
                (Some(file), Ok(meta)) => file.metadata().is_ok_and(|old| {
                    !meta.file_type().is_symlink()
                        && meta.dev() == old.dev()
                        && meta.ino() == old.ino()
                        && meta.file_type() == old.file_type()
                }),
                _ => false,
            };
            if !unchanged {
                return Err(denied(
                    "policy_stale",
                    "Protected configuration routes changed; install a new policy snapshot",
                ));
            }
        }
        Ok(())
    }
    fn relative(&self, path: &Path) -> Result<PathBuf, ToolFailure> {
        if path.as_os_str().as_encoded_bytes().len() > 4096 {
            return Err(denied("invalid_path", "Path exceeds policy byte limit"));
        }
        let path = if path.is_absolute() {
            path.strip_prefix(&self.workspace).map_err(|_| {
                denied(
                    "path_outside_workspace",
                    "Path is outside this policy workspace",
                )
            })?
        } else {
            path
        };
        let mut relative = PathBuf::new();
        for part in path.components() {
            match part {
                Component::Normal(value) => relative.push(value),
                Component::CurDir => {}
                Component::ParentDir => {
                    if !relative.pop() {
                        return Err(denied(
                            "path_outside_workspace",
                            "Path traverses outside workspace",
                        ));
                    }
                }
                _ => return Err(denied("invalid_path", "Unsupported path")),
            }
        }
        if relative.as_os_str().is_empty() {
            return Err(denied("invalid_path", "Mutation requires a file path"));
        }
        Ok(relative)
    }
    fn path(&self, path: &Path) -> Result<(), ToolFailure> {
        let relative = self.relative(path)?;
        if self
            .prefixes
            .iter()
            .any(|prefix| relative.starts_with(prefix))
        {
            return Err(denied(
                "protected_path",
                "The operation targets a protected workspace prefix",
            ));
        }
        Ok(())
    }
    fn effects(&self, effects: PreparedToolEffects<'_>) -> Result<Vec<PathBuf>, ToolFailure> {
        let PreparedToolEffects::FileMutation { paths } = effects else {
            return Err(denied(
                "opaque_effects",
                "This policy does not infer mutation or shell effects from arguments or preview text",
            ));
        };
        if paths.is_empty() || paths.len() > 32 {
            return Err(denied(
                "opaque_effects",
                "Prepared target count is unsupported",
            ));
        }
        for path in paths {
            if !path.is_absolute() {
                return Err(denied(
                    "opaque_effects",
                    "Prepared targets must be captured absolute paths",
                ));
            }
            self.path(path)?;
        }
        Ok(paths.to_vec())
    }
    pub fn wrap(self: &Arc<Self>, tool: Arc<dyn ExecutableTool>) -> GuardedTool {
        GuardedTool {
            tool,
            safety: self.clone(),
        }
    }
    pub async fn planning(&self) -> bool {
        self.mode.read().await.planning
    }
}
#[async_trait]
impl ContextHook for CodingSafety {
    async fn context(
        &self,
        input: ContextHookInput<'_>,
        _: ExtensionCancellation,
    ) -> Result<Option<String>> {
        self.workspace(input.workspace)
            .map_err(|e| anyhow::anyhow!(e.message))?;
        Ok(self.mode.read().await.planning.then(||"Planning mode: inspect and propose changes. Mutation and command tools are disabled by host policy; tool approvals remain separate.".into()))
    }
}
#[async_trait]
impl ToolHook for CodingSafety {
    async fn before_tool(
        &self,
        input: ToolHookInput<'_>,
        _: ExtensionCancellation,
    ) -> Result<ToolHookDecision> {
        self.workspace(input.workspace)
            .map_err(|e| anyhow::anyhow!(e.message))?;
        Ok(
            if self.mode.read().await.planning && input.capability != ToolCapability::ReadOnly {
                ToolHookDecision::Deny {
                    reason: "Planning blocks mutation and command capabilities".into(),
                }
            } else {
                ToolHookDecision::Continue
            },
        )
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PlanArguments {
    enabled: bool,
}
#[async_trait]
impl ExtensionCommand for CodingSafety {
    fn specification(&self) -> CommandSpec {
        CommandSpec {
            name: "plan".into(),
            description: "Change this host workspace's ephemeral planning policy".into(),
            parameters: json!({"type":"object","properties":{"enabled":{"type":"boolean"}},"required":["enabled"],"additionalProperties":false}),
        }
    }
    async fn execute(
        &self,
        request: CommandRequest<'_>,
        cancellation: ExtensionCancellation,
    ) -> Result<CommandOutput> {
        self.workspace(request.context.workspace)
            .map_err(|e| anyhow::anyhow!(e.message))?;
        let args: PlanArguments = serde_json::from_value(request.arguments.clone())
            .map_err(|_| anyhow::anyhow!("Expected only a boolean enabled field"))?;
        let mut state = self.mode.write().await;
        ensure!(
            !cancellation.is_cancelled(),
            "Planning command was cancelled"
        );
        state.epoch = state
            .epoch
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("Policy epoch exhausted"))?;
        state.planning = args.enabled;
        Ok(CommandOutput::Notice(format!(
            "Planning {}; ordinary approval still applies",
            if state.planning {
                "enabled"
            } else {
                "disabled"
            }
        )))
    }
}
pub struct GuardedTool {
    tool: Arc<dyn ExecutableTool>,
    safety: Arc<CodingSafety>,
}
struct GuardedPlan {
    inner: Box<dyn PreparedTool>,
    safety: Arc<CodingSafety>,
    epoch: u64,
    paths: Vec<PathBuf>,
}
#[async_trait]
impl ExecutableTool for GuardedTool {
    fn definition(&self) -> ToolDefinition {
        self.tool.definition()
    }
    fn capability(&self) -> ToolCapability {
        self.tool.capability()
    }
    async fn prepare(
        &self,
        context: &ToolContext,
        arguments: Value,
    ) -> Result<Option<Box<dyn PreparedTool>>, ToolFailure> {
        self.safety.workspace(context.workspace_root())?;
        if self.capability() == ToolCapability::ReadOnly {
            return self.tool.prepare(context, arguments).await;
        }
        let state = self.safety.mode.read().await;
        if state.planning {
            return Err(denied(
                "planning_mode",
                "Planning blocks preparation of mutations",
            ));
        }
        if let Some(path) = arguments.get("path").and_then(Value::as_str) {
            self.safety.path(Path::new(path))?;
        }
        self.safety.bindings()?;
        let inner = self
            .tool
            .prepare(context, arguments)
            .await?
            .ok_or_else(|| {
                denied(
                    "opaque_effects",
                    "Mutation requires an owned prepared operation",
                )
            })?;
        let paths = self.safety.effects(inner.effects())?;
        Ok(Some(Box::new(GuardedPlan {
            inner,
            safety: self.safety.clone(),
            epoch: state.epoch,
            paths,
        })))
    }
    async fn execute(
        &self,
        context: &ToolContext,
        arguments: Value,
    ) -> Result<ToolSuccess, ToolFailure> {
        self.safety.workspace(context.workspace_root())?;
        if self.capability() == ToolCapability::ReadOnly {
            return self.tool.execute(context, arguments).await;
        }
        self.prepare(context, arguments)
            .await?
            .ok_or_else(|| denied("opaque_effects", "Missing prepared operation"))?
            .execute(context)
            .await
    }
}
#[async_trait]
impl PreparedTool for GuardedPlan {
    fn effects(&self) -> PreparedToolEffects<'_> {
        PreparedToolEffects::FileMutation { paths: &self.paths }
    }
    fn preview(&self) -> ToolPreview {
        self.inner.preview()
    }
    async fn execute(self: Box<Self>, context: &ToolContext) -> Result<ToolSuccess, ToolFailure> {
        self.safety.workspace(context.workspace_root())?;
        let state = self.safety.mode.read().await;
        if state.planning || state.epoch != self.epoch {
            return Err(denied(
                "policy_changed",
                "Policy changed after preparation; prepare a new operation",
            ));
        }
        self.safety.bindings()?;
        for path in &self.paths {
            self.safety.path(path)?;
        }
        // Keep the read lease until the original owned operation has finished or is dropped.
        self.inner.execute(context).await
    }
}

//! Explicit host launch files, never project-discovered executable configuration or restored grants.
use crate::Runtime;
use aicoder_core::{
    TurnExecutionContext,
    extensions::{ExtensionRegistry, ExtensionSet},
    tools::ToolCapability,
};
use anyhow::{Result, ensure};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::Arc,
};
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
/// A bounded, explicitly selected launch file. Never discover this from project resources.
pub struct HostExtensionManifest {
    pub schema_version: u32,
    pub entries: Vec<HostExtensionEntry>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
/// Trusted executable settings; environment values are resolved only by the owning host.
pub struct HostExtensionEntry {
    pub id: String,
    pub kind: HostExtensionKind,
    pub program: PathBuf,
    #[serde(default)]
    pub arguments: Vec<String>,
    #[serde(default)]
    pub environment_references: BTreeMap<String, String>,
    #[serde(default)]
    pub aliases: BTreeMap<String, String>,
    #[serde(default)]
    pub capabilities: BTreeMap<String, ToolCapability>,
}
impl std::fmt::Debug for HostExtensionEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostExtensionEntry")
            .field("id", &self.id)
            .field("kind", &self.kind)
            .field("argument_count", &self.arguments.len())
            .field(
                "environment_reference_count",
                &self.environment_references.len(),
            )
            .finish_non_exhaustive()
    }
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HostExtensionKind {
    Process,
    McpStdio,
}
fn identifier(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 24
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
}
fn variable(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name
            .bytes()
            .enumerate()
            .all(|(i, b)| b == b'_' || b.is_ascii_alphabetic() || i > 0 && b.is_ascii_digit())
}
impl HostExtensionManifest {
    pub fn read(path: impl AsRef<Path>) -> Result<Self> {
        let (_, bytes) = crate::files::read_bounded(path.as_ref(), 256 * 1024, true)?
            .ok_or_else(|| anyhow::anyhow!("launch manifest missing"))?;
        let manifest: Self = serde_json::from_slice(&bytes)
            .map_err(|_| anyhow::anyhow!("invalid host extension manifest"))?;
        manifest.validate()?;
        Ok(manifest)
    }
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.schema_version == 1 && self.entries.len() <= 32,
            "unsupported host extension manifest version/count"
        );
        let mut ids = BTreeSet::new();
        for entry in &self.entries {
            ensure!(
                identifier(&entry.id)
                    && ids.insert(&entry.id)
                    && entry.program.is_absolute()
                    && entry.program.as_os_str().as_encoded_bytes().len() <= 4096,
                "invalid host extension identity/program"
            );
            ensure!(
                entry.arguments.len() <= 128
                    && entry.arguments.iter().map(String::len).sum::<usize>() <= 65536
                    && entry.arguments.iter().all(|s| !s.contains('\0')),
                "host argument budget exceeded"
            );
            ensure!(
                entry.environment_references.len() <= 128
                    && entry
                        .environment_references
                        .iter()
                        .all(|(a, b)| variable(a) && variable(b))
                    && entry.aliases.len() <= 128
                    && entry.capabilities.len() <= 128,
                "invalid host extension reference/catalog overrides"
            );
            ensure!(
                match entry.kind {
                    HostExtensionKind::Process => cfg!(feature = "process-extensions"),
                    HostExtensionKind::McpStdio => cfg!(feature = "mcp-stdio"),
                },
                "selected extension transport requires a build with host-extensions"
            );
        }
        Ok(())
    }
}
#[async_trait]
trait Owner: Send + Sync {
    fn close(&self);
    async fn shutdown(&self) -> bool;
}
#[cfg(feature = "process-extensions")]
struct ProcessOwner(crate::process_extensions::ProcessExtension);
#[cfg(feature = "process-extensions")]
#[async_trait]
impl Owner for ProcessOwner {
    fn close(&self) {
        self.0.abort();
    }
    async fn shutdown(&self) -> bool {
        let d = self.0.shutdown().await;
        d.reaped && d.worker_done && !d.cleanup_pending
    }
}
#[cfg(feature = "mcp-stdio")]
struct McpOwner(crate::mcp::McpExtension);
#[cfg(feature = "mcp-stdio")]
#[async_trait]
impl Owner for McpOwner {
    fn close(&self) {
        self.0.abort();
    }
    async fn shutdown(&self) -> bool {
        let d = self.0.shutdown().await;
        d.reaped && d.worker_done && !d.cleanup_pending
    }
}
/// Live process ownership for an immutable registration snapshot.
///
/// Keep this guard while using its tools and await [`Self::shutdown`] before runtime exit.
/// Dropping it invalidates retained adapters and signals their owned process groups; cleanup
/// continues independently of Tokio. This does not contain detached processes or native code.
pub struct ActivatedExtensions {
    extensions: Arc<ExtensionSet>,
    owners: Vec<Box<dyn Owner>>,
    scope: TurnExecutionContext,
}
impl ActivatedExtensions {
    /// Registrations only: cloning this handle does not extend executable authority.
    pub fn extensions(&self) -> Arc<ExtensionSet> {
        self.extensions.clone()
    }
    /// Cancel use, close transports and await every started owner's bounded cleanup report.
    pub async fn shutdown(&self) -> Result<()> {
        self.scope.cancel("host extensions shutting down");
        let mut complete = true;
        for owner in &self.owners {
            complete &= owner.shutdown().await;
        }
        ensure!(
            complete,
            "host extension cleanup incomplete; operations were not repeated"
        );
        Ok(())
    }
}
impl Drop for ActivatedExtensions {
    fn drop(&mut self) {
        self.scope.cancel("host extension owner dropped");
        for owner in &self.owners {
            owner.close();
        }
    }
}
impl Runtime {
    /// Resolve all launch settings, then connect and validate one complete immutable catalog.
    ///
    /// Failure publishes no snapshot and awaits started owners. Adoption remains explicit via
    /// [`Runtime::with_extensions`]; callers must retain the returned executable guard.
    pub async fn activate_extensions(
        &self,
        manifest: &HostExtensionManifest,
        context: TurnExecutionContext,
    ) -> Result<ActivatedExtensions> {
        manifest.validate()?;
        let scope = context.child();
        let mut activation = ActivatedExtensions {
            extensions: Arc::new(ExtensionSet::default()),
            owners: Vec::new(),
            scope: scope.clone(),
        };
        #[cfg(feature = "process-extensions")]
        let configs = manifest
            .entries
            .iter()
            .map(|entry| -> Result<_> {
                let mut config = crate::process_extensions::ProcessConfig::new(
                    &entry.program,
                    &self.config.workspace,
                    &entry.id,
                );
                config.arguments = entry
                    .arguments
                    .iter()
                    .map(|arg| arg.replace("{workspace}", &self.config.workspace.to_string_lossy()))
                    .collect();
                for (name, reference) in &entry.environment_references {
                    let value = self.environment.value(reference)?.ok_or_else(|| {
                        anyhow::anyhow!("host extension credential reference is unavailable")
                    })?;
                    config.environment.insert(name.clone(), value);
                }
                if entry.kind == HostExtensionKind::Process {
                    config.aliases = entry.aliases.clone();
                    config.capabilities = entry.capabilities.clone();
                }
                #[cfg(feature = "mcp-stdio")]
                if entry.kind == HostExtensionKind::McpStdio {
                    let mut mcp = crate::mcp::McpConfig::new(config.clone());
                    mcp.aliases = entry.aliases.clone();
                    mcp.capabilities = entry.capabilities.clone();
                    mcp.validate()?;
                }
                ensure!(
                    config.valid(),
                    "invalid effective host process configuration"
                );
                Ok((entry, config))
            })
            .collect::<Result<Vec<_>>>()?;
        #[cfg(feature = "process-extensions")]
        let mut registry = ExtensionRegistry::new();
        #[cfg(not(feature = "process-extensions"))]
        let registry = ExtensionRegistry::new();
        let result = async {
            #[cfg(feature = "process-extensions")]
            for (entry, mut config) in configs {
                if scope.is_cancelled() {
                    return Err(scope.error().into());
                }
                match entry.kind {
                    HostExtensionKind::Process => {
                        config.aliases = entry.aliases.clone();
                        config.capabilities = entry.capabilities.clone();
                        let process = crate::process_extensions::ProcessExtension::connect(
                            config,
                            scope.child(),
                        )
                        .await?;
                        let extension = process.extension();
                        activation.owners.push(Box::new(ProcessOwner(process)));
                        registry.install(extension?)?;
                    }
                    HostExtensionKind::McpStdio => {
                        #[cfg(feature = "mcp-stdio")]
                        {
                            let mut config = crate::mcp::McpConfig::new(config);
                            config.aliases = entry.aliases.clone();
                            config.capabilities = entry.capabilities.clone();
                            let process =
                                crate::mcp::McpExtension::connect(config, scope.child()).await?;
                            let extension = process.extension();
                            activation.owners.push(Box::new(McpOwner(process)));
                            registry.install(extension?)?;
                        }
                        #[cfg(not(feature = "mcp-stdio"))]
                        unreachable!("transport was checked before launch");
                    }
                }
            }
            let extensions = Arc::new(registry.finish());
            extensions.validate_tools(&self.default_registry()?)?;
            if let Some(current) = &self.extension_snapshot {
                current.combine(&extensions)?;
            }
            ensure!(!scope.is_cancelled(), "host extension activation cancelled");
            activation.extensions = extensions;
            Ok::<_, anyhow::Error>(())
        }
        .await;
        if let Err(error) = result {
            let cleanup = activation.shutdown().await;
            return Err(error.context(if cleanup.is_ok() {
                "host extension activation failed; started owners stopped"
            } else {
                "host extension activation failed; cleanup incomplete"
            }));
        }
        Ok(activation)
    }
}

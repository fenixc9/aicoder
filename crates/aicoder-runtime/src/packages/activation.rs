use super::*;
use aicoder_core::{
    TurnExecutionContext,
    extensions::{ExtensionRegistry, ExtensionSet},
};
use anyhow::{Context, Result, ensure};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
};
#[derive(Clone)]
pub struct HostProgram {
    pub program: PathBuf,
    pub version: String,
    pub environment_references: BTreeMap<String, String>,
}
impl HostProgram {
    pub fn new(program: impl Into<PathBuf>, version: impl Into<String>) -> Self {
        Self {
            program: program.into(),
            version: version.into(),
            environment_references: BTreeMap::new(),
        }
    }
}
impl std::fmt::Debug for HostProgram {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostProgram")
            .field("program", &self.program)
            .field("version", &self.version)
            .field(
                "environment_reference_names",
                &self.environment_references.keys().collect::<Vec<_>>(),
            )
            .finish()
    }
}
pub struct PackagePrograms {
    profiles: BTreeMap<String, HostProgram>,
    environment: Arc<dyn crate::Environment>,
}
impl PackagePrograms {
    pub fn new(environment: Arc<dyn crate::Environment>) -> Self {
        Self {
            profiles: BTreeMap::new(),
            environment,
        }
    }
    pub fn register(&mut self, name: impl Into<String>, profile: HostProgram) -> Result<()> {
        let name = name.into();
        super::manifest::name(&name)?;
        super::manifest::version(&profile.version)?;
        ensure!(
            profile.program.is_absolute()
                && profile.environment_references.len() <= 128
                && self.profiles.len() < 64
                && !self.profiles.contains_key(&name),
            "invalid/duplicate host program profile"
        );
        self.profiles.insert(name, profile);
        Ok(())
    }
}
enum Owner {
    Process(crate::process_extensions::ProcessExtension),
    #[cfg(feature = "mcp-stdio")]
    Mcp(crate::mcp::McpExtension),
}
impl Owner {
    fn abort(&self) {
        match self {
            Self::Process(p) => p.abort(),
            #[cfg(feature = "mcp-stdio")]
            Self::Mcp(p) => p.abort(),
        }
    }
    async fn shutdown(&self) -> crate::process_extensions::ProcessDiagnostics {
        match self {
            Self::Process(p) => p.shutdown().await,
            #[cfg(feature = "mcp-stdio")]
            Self::Mcp(p) => p.shutdown().await,
        }
    }
}
pub struct PackageActivation {
    extensions: Arc<ExtensionSet>,
    owners: Vec<Owner>,
    scope: TurnExecutionContext,
}
impl PackageActivation {
    pub fn extensions(&self) -> Arc<ExtensionSet> {
        self.extensions.clone()
    }
    pub async fn shutdown(&self) -> Result<()> {
        self.scope.cancel("package activation shutting down");
        let mut complete = true;
        for owner in &self.owners {
            let report = owner.shutdown().await;
            complete &= report.reaped && report.worker_done && !report.cleanup_pending;
        }
        ensure!(complete, "package process cleanup incomplete");
        Ok(())
    }
    pub(crate) fn abort(&self) {
        self.scope.cancel("package activation owner dropped");
        for owner in &self.owners {
            owner.abort();
        }
    }
}
impl Drop for PackageActivation {
    fn drop(&mut self) {
        self.abort();
    }
}
/// Owned read-only preparation; private launch settings never enter domain history.
pub(crate) struct PreparedPackageActivation {
    configs: Vec<(
        PackageProtocol,
        crate::process_extensions::ProcessConfig,
        String,
    )>,
}
impl PreparedPackageActivation {
    pub(crate) fn len(&self) -> usize {
        self.configs.len()
    }
}
impl PackageSnapshot {
    pub async fn activate(
        &self,
        store: &PackageStore,
        programs: &PackagePrograms,
        workspace: &Path,
        context: TurnExecutionContext,
    ) -> Result<PackageActivation> {
        self.prepare_activation(store, programs, workspace)?
            .activate(context)
            .await
    }
    pub(crate) fn prepare_activation(
        &self,
        store: &PackageStore,
        programs: &PackagePrograms,
        workspace: &Path,
    ) -> Result<PreparedPackageActivation> {
        self.validate_trust(store)?;
        let workspace = workspace.canonicalize()?;
        ensure!(
            self.objects
                .iter()
                .map(|p| p.manifest.executables.len())
                .sum::<usize>()
                <= 32,
            "package process count budget exceeded"
        );
        // Resolve every declaration/host reference before starting the first executable.
        let mut configs = Vec::new();
        for package in &self.objects {
            for entry in &package.manifest.executables {
                ensure!(
                    package.execution_allowed,
                    "package content has no execution consent"
                );
                let (program, environment) = match &entry.program {
                    PackageProgram::Bundled { path } => {
                        (package.root.path.join(path), BTreeMap::new())
                    }
                    PackageProgram::Dependency { name } => {
                        let expected = package
                            .manifest
                            .programs
                            .iter()
                            .find(|p| &p.name == name)
                            .context("program dependency is undeclared")?;
                        let profile = programs
                            .profiles
                            .get(name)
                            .context("host program profile is not configured")?;
                        ensure!(
                            profile.version == expected.version,
                            "host program version does not match the exact requirement"
                        );
                        let mut environment = BTreeMap::new();
                        for (target, reference) in &profile.environment_references {
                            let value = programs
                                .environment
                                .value(reference)
                                .map_err(|_| {
                                    anyhow::anyhow!(
                                        "host program environment reference lookup failed"
                                    )
                                })?
                                .context("host program environment reference is unavailable")?;
                            environment.insert(target.clone(), value);
                        }
                        (profile.program.clone(), environment)
                    }
                };
                let logical = format!("{}/{}", package.pin.id, entry.id);
                let digest = crate::resources::reader::sha256(logical.as_bytes());
                let namespace = format!(
                    "{}_{}",
                    package
                        .pin
                        .id
                        .bytes()
                        .filter(u8::is_ascii_alphanumeric)
                        .take(8)
                        .map(char::from)
                        .collect::<String>(),
                    &digest[..12]
                );
                let mut config =
                    crate::process_extensions::ProcessConfig::new(program, &workspace, namespace);
                config.environment = environment;
                config.arguments = entry
                    .arguments
                    .iter()
                    .map(|argument| {
                        argument
                            .replace("{package}", &package.root.path.to_string_lossy())
                            .replace("{workspace}", &workspace.to_string_lossy())
                    })
                    .collect();
                ensure!(
                    config.valid(),
                    "package launch declaration does not satisfy host limits"
                );
                #[cfg(not(feature = "mcp-stdio"))]
                ensure!(
                    !matches!(entry.protocol, PackageProtocol::Mcp),
                    "MCP package activation requires the mcp-stdio feature"
                );
                configs.push((
                    entry.protocol,
                    config,
                    format!("package-sha256:{}", package.pin.sha256),
                ));
            }
        }
        Ok(PreparedPackageActivation { configs })
    }
}
impl PreparedPackageActivation {
    pub(crate) async fn activate(self, context: TurnExecutionContext) -> Result<PackageActivation> {
        let scope = context.child();
        let mut activation = PackageActivation {
            extensions: Arc::new(ExtensionSet::default()),
            owners: Vec::new(),
            scope: scope.clone(),
        };
        let work = async {
            let mut registry = ExtensionRegistry::new();
            for (protocol, config, revision) in self.configs {
                if scope.is_cancelled() {
                    return Err(scope.error().into());
                }
                match protocol {
                    PackageProtocol::Process => {
                        let process = crate::process_extensions::ProcessExtension::connect(
                            config,
                            scope.child(),
                        )
                        .await?;
                        let contribution = process.extension_at_revision(&revision);
                        activation.owners.push(Owner::Process(process));
                        registry.install(contribution?)?;
                    }
                    PackageProtocol::Mcp => {
                        #[cfg(feature = "mcp-stdio")]
                        {
                            let mcp = crate::mcp::McpExtension::connect(
                                crate::mcp::McpConfig::new(config),
                                scope.child(),
                            )
                            .await?;
                            let contribution = mcp.extension_at_revision(&revision);
                            activation.owners.push(Owner::Mcp(mcp));
                            registry.install(contribution?)?;
                        }
                        #[cfg(not(feature = "mcp-stdio"))]
                        anyhow::bail!("MCP feature is unavailable");
                    }
                }
            }
            ensure!(!scope.is_cancelled(), "package activation cancelled");
            Ok::<_, anyhow::Error>(Arc::new(registry.finish()))
        }
        .await;
        match work {
            Ok(extensions) => {
                activation.extensions = extensions;
                Ok(activation)
            }
            Err(error) => {
                if activation.shutdown().await.is_ok() {
                    Err(error)
                } else {
                    Err(error
                        .context("package activation failed and process cleanup is incomplete"))
                }
            }
        }
    }
}

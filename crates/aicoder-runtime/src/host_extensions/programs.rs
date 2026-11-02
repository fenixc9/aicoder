use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

/// Explicit host program profiles for activating already installed, trusted packages.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostProgramManifest {
    pub schema_version: u32,
    pub profiles: BTreeMap<String, HostProgramProfile>,
}
/// No credential values or package-selected host paths are stored in a profile.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostProgramProfile {
    pub program: PathBuf,
    pub version: String,
    #[serde(default)]
    pub environment_references: BTreeMap<String, String>,
}
impl std::fmt::Debug for HostProgramProfile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostProgramProfile")
            .field("reference_count", &self.environment_references.len())
            .finish_non_exhaustive()
    }
}
impl Default for HostProgramManifest {
    fn default() -> Self {
        Self {
            schema_version: 1,
            profiles: BTreeMap::new(),
        }
    }
}
impl HostProgramManifest {
    pub fn read(path: impl AsRef<Path>) -> Result<Self> {
        let (_, bytes) = crate::files::read_bounded(path.as_ref(), 256 * 1024, true)?
            .ok_or_else(|| anyhow::anyhow!("host program manifest missing"))?;
        let manifest: Self = serde_json::from_slice(&bytes)
            .map_err(|_| anyhow::anyhow!("invalid host program manifest"))?;
        manifest.validate()?;
        Ok(manifest)
    }
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.schema_version == 1 && self.profiles.len() <= 64,
            "unsupported host program manifest version/count"
        );
        for (name, profile) in &self.profiles {
            ensure!(
                !name.is_empty()
                    && name.len() <= 64
                    && name != "."
                    && name != ".."
                    && name
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b)),
                "invalid host program profile name"
            );
            ensure!(
                !profile.version.is_empty()
                    && profile.version.len() <= 128
                    && profile.version.bytes().all(|b| b.is_ascii_graphic())
                    && profile.program.is_absolute()
                    && profile.program.as_os_str().as_encoded_bytes().len() <= 4096
                    && !profile.program.as_os_str().as_encoded_bytes().contains(&0),
                "invalid host program profile identity"
            );
            ensure!(
                profile.environment_references.len() <= 128
                    && profile
                        .environment_references
                        .iter()
                        .all(|(a, b)| super::variable(a) && super::variable(b)),
                "invalid host program environment references"
            );
        }
        Ok(())
    }
    #[cfg(feature = "process-extensions")]
    pub(super) fn resolve(
        &self,
        environment: std::sync::Arc<dyn crate::Environment>,
    ) -> Result<crate::packages::PackagePrograms> {
        self.validate()?;
        let mut programs = crate::packages::PackagePrograms::new(environment);
        for (name, profile) in &self.profiles {
            let mut program = crate::packages::HostProgram::new(&profile.program, &profile.version);
            program.environment_references = profile.environment_references.clone();
            programs.register(name, program)?;
        }
        Ok(programs)
    }
}

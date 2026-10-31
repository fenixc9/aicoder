//! Explicit content-pinned resource bundles. Registry trust is host state, never model history.
#[cfg(feature = "process-extensions")]
mod activation;
mod manifest;
mod snapshot;
mod state;
mod store;
use crate::Setting;
#[cfg(feature = "process-extensions")]
pub use activation::{HostProgram, PackageActivation, PackagePrograms};
use anyhow::{Result, ensure};
pub use manifest::{
    MANIFEST_FILE, PackageExecutable, PackageManifest, PackagePermissions, PackagePin,
    PackageProgram, PackageProtocol, PackageResources, ProgramRequirement, StateField,
};
use serde::{Deserialize, Serialize};
pub use snapshot::{PackageObject, PackageSnapshot};
pub use state::{PACKAGE_STATE_NAMESPACE, PackageSessionState};
pub use store::{
    InstallOptions, PackageIndex, PackageLimits, PackageRecord, PackageSource, PackageStore,
};
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PackageSettings {
    #[serde(skip_serializing_if = "Setting::is_inherit")]
    pub enabled: Setting<bool>,
    #[serde(skip_serializing_if = "Setting::is_inherit")]
    pub selected: Setting<Vec<String>>,
}
#[derive(Debug, Clone, Serialize)]
pub struct PackagePolicy {
    pub enabled: bool,
    pub selected: Option<Vec<String>>,
}
impl Default for PackagePolicy {
    fn default() -> Self {
        Self {
            enabled: true,
            selected: None,
        }
    }
}
impl PackagePolicy {
    pub(crate) fn validate(&self) -> Result<()> {
        if let Some(names) = &self.selected {
            ensure!(names.len() <= 32, "package selection budget exceeded");
            let mut seen = std::collections::BTreeSet::new();
            for name in names {
                manifest::name(name)?;
                ensure!(seen.insert(name), "duplicate selected package");
            }
        }
        Ok(())
    }
}

pub(crate) fn bounded_json(value: &impl Serialize, limit: usize) -> Result<()> {
    struct Count {
        bytes: usize,
        limit: usize,
    }
    impl std::io::Write for Count {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > self.limit.saturating_sub(self.bytes) {
                return Err(std::io::Error::other(
                    "package metadata byte budget exceeded",
                ));
            }
            self.bytes += bytes.len();
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    serde_json::to_writer(&mut Count { bytes: 0, limit }, value)?;
    Ok(())
}

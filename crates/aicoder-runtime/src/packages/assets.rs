use super::{PackagePin, PackageStore, manifest};
use anyhow::{Context, Result, ensure};
use std::sync::Arc;

/// An explicit immutable package asset snapshot. Contents are data, never execution authority.
#[derive(Clone)]
pub struct PackageAsset {
    pin: PackagePin,
    path: String,
    bytes: Arc<[u8]>,
}
impl PackageAsset {
    pub fn pin(&self) -> &PackagePin {
        &self.pin
    }
    pub fn path(&self) -> &str {
        &self.path
    }
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}
impl std::fmt::Debug for PackageAsset {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PackageAsset")
            .field("pin", &self.pin)
            .field("path", &self.path)
            .field("bytes", &self.bytes.len())
            .finish()
    }
}
impl crate::Runtime {
    /// Load one explicitly named declared theme, including when automatic resource discovery
    /// is disabled. The host package policy and current installed trust still apply.
    /// No installation, executable activation, credential lookup or model request occurs.
    pub fn package_theme_asset(
        &self,
        package: &str,
        path: &str,
        limit: usize,
    ) -> Result<PackageAsset> {
        manifest::name(package)?;
        manifest::relative(path)?;
        ensure!(
            limit <= 32 * 1024 * 1024,
            "package theme read budget exceeded"
        );
        ensure!(
            self.config.packages.enabled,
            "packages are disabled by the host"
        );
        let root = self
            .config
            .state_root
            .as_ref()
            .context("package themes require host storage")?
            .join("packages");
        let store = PackageStore::existing(&root)?.context("theme package is not installed")?;
        let snapshot = store.snapshot(Some(&[package.to_owned()]), None)?;
        let object = snapshot
            .get(package)
            .context("theme package is not installed")?;
        ensure!(
            object
                .manifest
                .resources
                .themes
                .iter()
                .any(|declared| declared == path),
            "asset is not a declared package theme"
        );
        Ok(PackageAsset {
            pin: object.pin.clone(),
            path: path.into(),
            bytes: object.read_asset(path, limit)?.into(),
        })
    }
}

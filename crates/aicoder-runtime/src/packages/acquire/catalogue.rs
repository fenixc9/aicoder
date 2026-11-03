use super::super::PackagePin;
use super::*;

/// Host-selected source addresses for exact dependency content. No credential/profile routing.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageSourceCatalogue {
    pub schema_version: u32,
    pub entries: Vec<PackageSourceEntry>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageSourceEntry {
    pub pin: PackagePin,
    pub request: PackageRequest,
}
impl Default for PackageSourceCatalogue {
    fn default() -> Self {
        Self {
            schema_version: 1,
            entries: Vec::new(),
        }
    }
}
impl PackageSourceCatalogue {
    pub fn read(path: impl AsRef<Path>) -> Result<Self> {
        let (_, bytes) = crate::files::read_bounded(path.as_ref(), 256 * 1024, true)?
            .ok_or_else(|| anyhow::anyhow!("package source catalogue missing"))?;
        let result: Self = serde_json::from_slice(&bytes)
            .map_err(|_| anyhow::anyhow!("invalid package source catalogue"))?;
        result.validate()?;
        Ok(result)
    }
    fn validate(&self) -> Result<()> {
        ensure!(
            self.schema_version == 1 && self.entries.len() <= 32,
            "source catalogue version/count budget exceeded"
        );
        let mut pins = BTreeSet::new();
        for entry in &self.entries {
            entry.pin.validate()?;
            ensure!(
                pins.insert((entry.pin.id.clone(), entry.pin.sha256.clone())),
                "duplicate source catalogue pin"
            );
        }
        Ok(())
    }
}
impl PackageAcquirer {
    /// Fetch an exact dependency closure using only host-supplied addresses, then atomically
    /// publish the selected root and installed closure against the observed registry revision.
    pub async fn install_with_sources(
        &self,
        store: &PackageStore,
        request: &PackageRequest,
        catalogue: &PackageSourceCatalogue,
        options: InstallOptions,
        context: TurnExecutionContext,
    ) -> Result<PackageRecord> {
        self.limits.validate()?;
        catalogue.validate()?;
        self.validate_request(request)?;
        for entry in &catalogue.entries {
            self.validate_request(&entry.request)?;
        }
        let index = store.index()?;
        let started = Instant::now();
        let mut root_limits = self.limits;
        root_limits.max_download_bytes = root_limits
            .max_download_bytes
            .min(self.limits.max_graph_bytes);
        root_limits.max_unpacked_bytes = root_limits
            .max_unpacked_bytes
            .min(self.limits.max_graph_bytes);
        let root_client = self.limited(root_limits);
        let (root, downloaded, decoded) = root_client
            .prepare(store, request, options, &context, started)
            .await?;
        let pin = root.record.pin.clone();
        let mut prepared = vec![root];
        let mut pending = prepared[0].manifest.dependencies.clone();
        let mut selected = BTreeMap::from([(pin.id.clone(), pin.sha256.clone())]);
        let mut downloaded = downloaded;
        let mut decoded = decoded;
        while let Some(dependency) = pending.pop() {
            check(&context, started, self.limits.timeout)?;
            if let Some(selected) = selected.get(&dependency.id) {
                ensure!(
                    selected == &dependency.sha256,
                    "dependency closure selects conflicting revisions"
                );
                continue;
            }
            ensure!(
                selected.len() < self.limits.max_graph_packages,
                "dependency closure count budget exceeded"
            );
            selected.insert(dependency.id.clone(), dependency.sha256.clone());
            if let Some(record) = index
                .installed
                .get(&dependency.sha256)
                .filter(|r| r.pin == dependency)
            {
                let object = store.object(&dependency)?;
                ensure!(
                    !object.manifest.permissions.execution || record.execution_allowed,
                    "installed dependency execution trust is unavailable"
                );
                pending.extend(object.manifest.dependencies.clone());
                continue;
            }
            let source = catalogue
                .entries
                .iter()
                .find(|entry| entry.pin == dependency)
                .ok_or_else(|| {
                    anyhow::anyhow!("exact dependency has no host source catalogue entry")
                })?;
            let mut limits = self.limits;
            limits.max_download_bytes = limits
                .max_download_bytes
                .min(self.limits.max_graph_bytes.saturating_sub(downloaded));
            limits.max_unpacked_bytes = limits
                .max_unpacked_bytes
                .min(self.limits.max_graph_bytes.saturating_sub(decoded));
            limits.validate()?;
            let limited = self.limited(limits);
            let (item, bytes, unpacked) = limited
                .prepare(store, &source.request, options, &context, started)
                .await?;
            ensure!(
                item.record.pin == dependency,
                "downloaded dependency differs from its exact content pin"
            );
            downloaded = downloaded
                .checked_add(bytes)
                .ok_or_else(|| anyhow::anyhow!("graph download overflow"))?;
            decoded = decoded
                .checked_add(unpacked)
                .ok_or_else(|| anyhow::anyhow!("graph decoded-byte overflow"))?;
            ensure!(
                downloaded <= self.limits.max_graph_bytes && decoded <= self.limits.max_graph_bytes,
                "dependency closure byte budget exceeded"
            );
            pending.extend(item.manifest.dependencies.clone());
            prepared.push(item);
        }
        check(&context, started, self.limits.timeout)?;
        store.commit_prepared(
            prepared,
            &pin,
            options.enable,
            Some(index.revision),
            &context,
        )
    }
    fn validate_request(&self, request: &PackageRequest) -> Result<()> {
        match request {
            PackageRequest::Git { url, revision } => {
                self.url(url)?;
                ensure!(
                    !revision.is_empty()
                        && revision.len() <= 256
                        && !revision.starts_with('-')
                        && revision
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b"/._-".contains(&b)),
                    "invalid Git revision"
                );
            }
            PackageRequest::Npm {
                registry,
                package,
                version,
            } => {
                self.url(registry)?;
                npm_name(package)?;
                label(version.as_deref().unwrap_or("latest"))?;
            }
        }
        Ok(())
    }
    fn limited(&self, limits: PackageAcquireLimits) -> Self {
        Self {
            client: self.client.clone(),
            limits,
            loopback_http: self.loopback_http,
            environment: self.environment.clone(),
            bearer_references: self.bearer_references.clone(),
        }
    }
}

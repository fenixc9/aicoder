//! Explicit bounded acquisition. Source data never selects credentials or launches package code.
mod archive;
mod git;
use super::{InstallOptions, PackageRecord, PackageSource, PackageStore};
use crate::resources::reader::Root;
use aicoder_core::TurnExecutionContext;
use anyhow::{Result, ensure};
use archive::extract;
use base64::Engine;
use git::git_archive;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{Cursor, Read, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};
use url::Url;

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PackageRequest {
    Git {
        url: String,
        revision: String,
    },
    Npm {
        registry: String,
        package: String,
        version: Option<String>,
    },
}
impl std::fmt::Debug for PackageRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Git { .. } => "PackageRequest::Git",
            Self::Npm { .. } => "PackageRequest::Npm",
        })
    }
}
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct PackageAcquireLimits {
    pub max_download_bytes: usize,
    pub max_unpacked_bytes: usize,
    pub max_entries: usize,
    pub git_disk_bytes: u64,
    pub timeout: Duration,
}
impl Default for PackageAcquireLimits {
    fn default() -> Self {
        Self {
            max_download_bytes: 64 * 1024 * 1024,
            max_unpacked_bytes: 128 * 1024 * 1024,
            max_entries: 16384,
            git_disk_bytes: 256 * 1024 * 1024,
            timeout: Duration::from_secs(120),
        }
    }
}
impl PackageAcquireLimits {
    fn validate(self) -> Result<()> {
        ensure!(
            (1024..=128 * 1024 * 1024).contains(&self.max_download_bytes)
                && (1024..=256 * 1024 * 1024).contains(&self.max_unpacked_bytes)
                && (1..=32768).contains(&self.max_entries)
                && (16 * 1024 * 1024..=512 * 1024 * 1024).contains(&self.git_disk_bytes)
                && (Duration::from_millis(1)..=Duration::from_secs(600)).contains(&self.timeout),
            "invalid package acquisition limits"
        );
        Ok(())
    }
}
pub struct PackageAcquirer {
    client: reqwest::Client,
    limits: PackageAcquireLimits,
    loopback_http: bool,
    environment: Arc<dyn crate::Environment>,
    bearer_references: BTreeMap<String, String>,
}
impl PackageAcquirer {
    pub fn new(limits: PackageAcquireLimits) -> Result<Self> {
        limits.validate()?;
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .timeout(limits.timeout)
            .no_proxy()
            .build()
            .map_err(|_| anyhow::anyhow!("cannot build package download client"))?;
        Ok(Self {
            client,
            limits,
            loopback_http: false,
            environment: Arc::new(crate::ProcessEnvironment),
            bearer_references: BTreeMap::new(),
        })
    }
    /// Use the host's layered environment without resolving any value until an explicitly
    /// bound origin is requested. Package metadata cannot create credential bindings.
    pub fn for_runtime(runtime: &crate::Runtime, limits: PackageAcquireLimits) -> Result<Self> {
        let mut client = Self::new(limits)?;
        client.environment = runtime.environment.clone();
        Ok(client)
    }
    pub fn with_environment(mut self, environment: Arc<dyn crate::Environment>) -> Self {
        self.environment = environment;
        self
    }
    pub fn with_bearer_reference(mut self, origin: &str, reference: &str) -> Result<Self> {
        let url = self.url(origin)?;
        ensure!(
            self.bearer_references.len() < 8
                && !reference.is_empty()
                && reference.len() <= 128
                && reference.bytes().enumerate().all(|(i, b)| b == b'_'
                    || b.is_ascii_alphabetic()
                    || i > 0 && b.is_ascii_digit()),
            "invalid package credential reference binding"
        );
        ensure!(
            self.bearer_references
                .insert(url.origin().ascii_serialization(), reference.into())
                .is_none(),
            "duplicate package credential origin"
        );
        Ok(self)
    }
    fn bearer(&self, url: &Url) -> Result<Option<String>> {
        let Some(reference) = self
            .bearer_references
            .get(&url.origin().ascii_serialization())
        else {
            return Ok(None);
        };
        let value = self
            .environment
            .value(reference)
            .map_err(|_| anyhow::anyhow!("package credential lookup failed"))?
            .ok_or_else(|| anyhow::anyhow!("package credential reference unavailable"))?;
        ensure!(
            !value.is_empty() && value.len() <= 8192,
            "invalid package credential value"
        );
        reqwest::header::HeaderValue::from_str(&format!("Bearer {value}"))
            .map_err(|_| anyhow::anyhow!("invalid package credential header"))?;
        Ok(Some(value))
    }
    /// Explicit host policy for loopback registry/Git fixtures; never accepts arbitrary HTTP.
    pub fn with_loopback_http(mut self, enabled: bool) -> Self {
        self.loopback_http = enabled;
        self
    }
    fn url(&self, value: &str) -> Result<Url> {
        ensure!(value.len() <= 4096, "package URL exceeds limit");
        let url = Url::parse(value).map_err(|_| anyhow::anyhow!("invalid package URL"))?;
        let local = match url.host() {
            Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
            Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
            Some(url::Host::Domain(name)) => name == "localhost",
            _ => false,
        };
        ensure!(
            url.scheme() == "https" || self.loopback_http && local && url.scheme() == "http",
            "package transport must use HTTPS"
        );
        ensure!(
            url.username().is_empty()
                && url.password().is_none()
                && url.query().is_none()
                && url.fragment().is_none(),
            "package URLs cannot carry credentials, queries or fragments"
        );
        Ok(url)
    }
    /// Network and private extraction complete before the existing atomic import publishes trust.
    pub async fn install(
        &self,
        store: &PackageStore,
        request: &PackageRequest,
        options: InstallOptions,
        context: TurnExecutionContext,
    ) -> Result<PackageRecord> {
        self.limits.validate()?;
        let started = Instant::now();
        check(&context, started, self.limits.timeout)?;
        let (bytes, origin, gzip) = match request {
            PackageRequest::Npm {
                registry,
                package,
                version,
            } => {
                self.npm(registry, package, version.as_deref(), &context, started)
                    .await?
            }
            PackageRequest::Git { url, revision } => {
                self.git(url, revision, &context, started).await?
            }
        };
        check(&context, started, self.limits.timeout)?;
        let stage = super::store::Stage(
            store
                .root
                .join("staging")
                .join(format!("acquire-{}", uuid::Uuid::new_v4())),
        );
        ensure!(
            fs2::available_space(&store.root)?
                >= (self.limits.max_unpacked_bytes as u64).saturating_add(64 * 1024 * 1024),
            "insufficient package extraction disk admission"
        );
        fs::create_dir(&stage.0)?;
        fs::set_permissions(&stage.0, fs::Permissions::from_mode(0o700))?;
        extract(&bytes, gzip, &stage.0, self.limits, &context, started)?;
        if let PackageSource::Npm {
            package, version, ..
        } = &origin
        {
            let raw = Root::open(&stage.0)?.read(Path::new("package.json"), 1024 * 1024)?;
            let identity: NpmIdentity = serde_json::from_slice(&raw)
                .map_err(|_| anyhow::anyhow!("invalid downloaded npm package identity"))?;
            ensure!(
                &identity.name == package && &identity.version == version,
                "downloaded npm identity differs from resolved metadata"
            );
        }
        check(&context, started, self.limits.timeout)?;
        store.import(&Root::open(&stage.0)?, origin, options, &context)
    }
    async fn body(
        &self,
        url: Url,
        limit: usize,
        context: &TurnExecutionContext,
        started: Instant,
    ) -> Result<Vec<u8>> {
        let token = self.bearer(&url)?;
        let mut request = self.client.get(url);
        if let Some(token) = token {
            request = request.bearer_auth(token);
        }
        let request = request.send();
        let mut response = tokio::select! {
            _ = context.cancelled() => return Err(context.error().into()),
            result = tokio::time::timeout(remaining(started, self.limits.timeout)?, request) =>
                result.map_err(|_| anyhow::anyhow!("package acquisition deadline exceeded"))?
                    .map_err(|_| anyhow::anyhow!("package request failed"))?,
        };
        ensure!(
            response.status().is_success(),
            "package server rejected the request"
        );
        ensure!(
            response.content_length().is_none_or(|n| n <= limit as u64),
            "package download exceeds byte budget"
        );
        let mut bytes = Vec::new();
        loop {
            check(context, started, self.limits.timeout)?;
            let chunk = tokio::select! {
                _ = context.cancelled() => return Err(context.error().into()),
                chunk = tokio::time::timeout(remaining(started, self.limits.timeout)?, response.chunk()) =>
                    chunk.map_err(|_| anyhow::anyhow!("package acquisition deadline exceeded"))?
                        .map_err(|_| anyhow::anyhow!("package response incomplete"))?,
            };
            let Some(chunk) = chunk else {
                break;
            };
            ensure!(
                chunk.len() <= limit.saturating_sub(bytes.len()),
                "package download byte budget exhausted"
            );
            bytes.extend_from_slice(&chunk);
        }
        Ok(bytes)
    }
    async fn npm(
        &self,
        registry: &str,
        package: &str,
        version: Option<&str>,
        context: &TurnExecutionContext,
        started: Instant,
    ) -> Result<(Vec<u8>, PackageSource, bool)> {
        npm_name(package)?;
        let requested = version.unwrap_or("latest");
        label(requested)?;
        let registry = self.url(registry)?;
        let mut metadata_url = registry.clone();
        metadata_url
            .path_segments_mut()
            .map_err(|_| anyhow::anyhow!("invalid registry base"))?
            .pop_if_empty()
            .push(package)
            .push(requested);
        let raw = self
            .body(metadata_url, 1024 * 1024, context, started)
            .await?;
        let metadata: NpmVersion = serde_json::from_slice(&raw)
            .map_err(|_| anyhow::anyhow!("invalid npm version metadata"))?;
        ensure!(metadata.name == package, "npm package identity mismatch");
        ensure!(
            concrete_version(&metadata.version),
            "npm metadata must resolve to a concrete version"
        );
        if concrete_version(requested) {
            ensure!(
                metadata.version == requested,
                "npm resolved version differs from requested version"
            );
        }
        let target = self.url(&metadata.dist.tarball)?;
        ensure!(
            target.origin() == registry.origin(),
            "npm tarball origin is not the host-selected registry"
        );
        let bytes = self
            .body(target, self.limits.max_download_bytes, context, started)
            .await?;
        let integrity = verify_integrity(&metadata.dist, &bytes)?;
        let origin = PackageSource::Npm {
            registry: registry.to_string(),
            package: package.into(),
            version: metadata.version,
            integrity,
            archive_sha256: crate::resources::reader::sha256(&bytes),
        };
        Ok((bytes, origin, true))
    }
    async fn git(
        &self,
        url: &str,
        revision: &str,
        context: &TurnExecutionContext,
        started: Instant,
    ) -> Result<(Vec<u8>, PackageSource, bool)> {
        let url = self.url(url)?;
        ensure!(
            !revision.is_empty()
                && revision.len() <= 256
                && !revision.starts_with('-')
                && revision
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"/._-".contains(&b)),
            "invalid Git revision"
        );
        let token = self.bearer(&url)?;
        let output = git_archive(
            &url,
            revision,
            self.limits,
            token.as_deref(),
            context.child(),
        )
        .await?;
        check(context, started, self.limits.timeout)?;
        let newline = output
            .iter()
            .position(|b| *b == b'\n')
            .ok_or_else(|| anyhow::anyhow!("Git revision receipt missing"))?;
        let commit = std::str::from_utf8(&output[..newline])
            .map_err(|_| anyhow::anyhow!("invalid Git revision receipt"))?;
        ensure!(
            (commit.len() == 40 || commit.len() == 64)
                && commit.bytes().all(|b| b.is_ascii_hexdigit()),
            "invalid resolved Git commit"
        );
        let bytes = output[newline + 1..].to_vec();
        ensure!(
            bytes.len() <= self.limits.max_download_bytes,
            "Git archive byte budget exhausted"
        );
        let origin = PackageSource::Git {
            url: url.to_string(),
            requested_revision: revision.into(),
            commit: commit.into(),
            archive_sha256: crate::resources::reader::sha256(&bytes),
        };
        Ok((bytes, origin, false))
    }
}
fn label(value: &str) -> Result<()> {
    ensure!(
        !value.is_empty()
            && value.len() <= 128
            && value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._+-".contains(&b)),
        "invalid npm version/tag"
    );
    Ok(())
}
fn npm_name(value: &str) -> Result<()> {
    ensure!(
        value.len() <= 214 && !value.is_empty(),
        "invalid npm package name"
    );
    let parts = value
        .strip_prefix('@')
        .map_or_else(|| vec![value], |v| v.split('/').collect());
    ensure!(
        (!value.starts_with('@') && parts.len() == 1 || value.starts_with('@') && parts.len() == 2)
            && parts.iter().all(|p| !p.is_empty()
                && *p != "."
                && *p != ".."
                && p.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))),
        "invalid npm package name"
    );
    Ok(())
}
#[derive(Deserialize)]
struct NpmVersion {
    name: String,
    version: String,
    dist: NpmDist,
}
#[derive(Deserialize)]
struct NpmIdentity {
    name: String,
    version: String,
}
fn concrete_version(value: &str) -> bool {
    if value.len() > 128 {
        return false;
    }
    let (main, build) = value
        .split_once('+')
        .map_or((value, None), |(a, b)| (a, Some(b)));
    let (core, pre) = main
        .split_once('-')
        .map_or((main, None), |(a, b)| (a, Some(b)));
    let identifiers = |v: &str| {
        !v.is_empty()
            && v.split('.').all(|part| {
                !part.is_empty() && part.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
            })
    };
    let numeric = |v: &str| {
        !v.is_empty()
            && v.bytes().all(|b| b.is_ascii_digit())
            && (v.len() == 1 || !v.starts_with('0'))
    };
    let parts = core.split('.').collect::<Vec<_>>();
    parts.len() == 3
        && parts.iter().all(|p| numeric(p))
        && build.is_none_or(identifiers)
        && pre.is_none_or(|v| {
            identifiers(v)
                && v.split('.')
                    .all(|p| !p.bytes().all(|b| b.is_ascii_digit()) || numeric(p))
        })
}
#[derive(Deserialize)]
struct NpmDist {
    tarball: String,
    integrity: Option<String>,
    shasum: Option<String>,
}
fn verify_integrity(dist: &NpmDist, bytes: &[u8]) -> Result<String> {
    if let Some(value) = &dist.integrity {
        ensure!(value.len() <= 4096, "npm integrity budget exceeded");
        for (prefix, algorithm) in [
            ("sha512-", &ring::digest::SHA512),
            ("sha256-", &ring::digest::SHA256),
        ] {
            let candidates = value
                .split_ascii_whitespace()
                .filter_map(|v| v.strip_prefix(prefix))
                .collect::<Vec<_>>();
            if !candidates.is_empty() {
                let expected = ring::digest::digest(algorithm, bytes);
                let matches = candidates.iter().any(|encoded| {
                    base64::engine::general_purpose::STANDARD
                        .decode(encoded)
                        .is_ok_and(|v| v == expected.as_ref())
                });
                ensure!(matches, "npm archive integrity mismatch");
                return Ok(format!(
                    "{prefix}{}",
                    base64::engine::general_purpose::STANDARD.encode(expected.as_ref())
                ));
            }
        }
        anyhow::bail!("npm integrity algorithm is unsupported");
    }
    let expected = dist
        .shasum
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("npm archive has no integrity digest"))?;
    ensure!(
        expected.len() == 40 && expected.bytes().all(|b| b.is_ascii_hexdigit()),
        "invalid npm legacy digest"
    );
    let actual = ring::digest::digest(&ring::digest::SHA1_FOR_LEGACY_USE_ONLY, bytes);
    let hex = actual
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    ensure!(
        hex.eq_ignore_ascii_case(expected),
        "npm legacy archive integrity mismatch"
    );
    Ok(format!(
        "sha1-{}",
        base64::engine::general_purpose::STANDARD.encode(actual.as_ref())
    ))
}
fn check(context: &TurnExecutionContext, started: Instant, limit: Duration) -> Result<()> {
    if context.is_cancelled() {
        return Err(context.error().into());
    }
    ensure!(
        started.elapsed() < limit,
        "package acquisition deadline exceeded"
    );
    Ok(())
}
fn remaining(started: Instant, limit: Duration) -> Result<Duration> {
    limit
        .checked_sub(started.elapsed())
        .filter(|d| !d.is_zero())
        .ok_or_else(|| anyhow::anyhow!("package acquisition deadline exceeded"))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn tar_bytes(entries: &[(&str, u8, &[u8])]) -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        for (path, kind, body) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(tar::EntryType::new(*kind));
            header.set_mode(0o7777);
            header.set_size(body.len() as u64);
            header.as_mut_bytes()[..100].fill(0);
            header.as_mut_bytes()[..path.len()].copy_from_slice(path.as_bytes());
            header.set_cksum();
            builder.append(&header, *body).unwrap();
        }
        builder.into_inner().unwrap()
    }
    fn rejects(bytes: &[u8], gzip: bool, limits: PackageAcquireLimits) {
        let root = tempfile::tempdir().unwrap();
        assert!(
            extract(
                bytes,
                gzip,
                root.path(),
                limits,
                &TurnExecutionContext::new(),
                Instant::now()
            )
            .is_err()
        );
    }
    #[test]
    fn archive_rejects_traversal_aliases_duplicates_links_devices_and_sparse_types() {
        for path in [
            "../escape",
            "/absolute",
            "package/../escape",
            "package//alias",
            "package/./alias",
            "package/a\\b",
        ] {
            rejects(
                &tar_bytes(&[(path, b'0', b"value")]),
                false,
                PackageAcquireLimits::default(),
            );
        }
        for kind in [b'1', b'2', b'3', b'4', b'6', b'S', b'K'] {
            rejects(
                &tar_bytes(&[("package/value", kind, b"")]),
                false,
                PackageAcquireLimits::default(),
            );
        }
        rejects(
            &tar_bytes(&[
                ("package/value", b'0', b"first"),
                ("package/value", b'0', b"second"),
            ]),
            false,
            PackageAcquireLimits::default(),
        );
    }
    #[test]
    fn archive_admits_bounded_long_paths_but_not_dangling_or_escaping_metadata() {
        let root = tempfile::tempdir().unwrap();
        let path = format!("package/{}.json", "long".repeat(40));
        let bytes = tar_bytes(&[
            ("././@LongLink", b'L', format!("{path}\0").as_bytes()),
            ("package/short", b'0', b"value"),
        ]);
        extract(
            &bytes,
            false,
            root.path(),
            PackageAcquireLimits::default(),
            &TurnExecutionContext::new(),
            Instant::now(),
        )
        .unwrap();
        assert_eq!(
            fs::read(root.path().join(path.strip_prefix("package/").unwrap())).unwrap(),
            b"value"
        );
        rejects(
            &tar_bytes(&[
                ("././@LongLink", b'L', b"package/../escape\0"),
                ("package/short", b'0', b"value"),
            ]),
            false,
            PackageAcquireLimits::default(),
        );
        rejects(
            &tar_bytes(&[("././@LongLink", b'L', b"package/no-next\0")]),
            false,
            PackageAcquireLimits::default(),
        );
        rejects(
            &tar_bytes(&[
                ("meta", b'x', b"20 path=../../evil\n"),
                ("package/value", b'0', b"x"),
            ]),
            false,
            PackageAcquireLimits::default(),
        );
    }
    #[test]
    fn extraction_bounds_entries_inflation_trailing_bytes_and_permissions() {
        let limits = PackageAcquireLimits {
            max_entries: 1,
            ..Default::default()
        };
        rejects(
            &tar_bytes(&[("package/a", b'0', b"a"), ("package/b", b'0', b"b")]),
            false,
            limits,
        );
        let raw = tar_bytes(&[("package/value", b'0', &vec![b'x'; 65536])]);
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
        encoder.write_all(&raw).unwrap();
        let compressed = encoder.finish().unwrap();
        let limits = PackageAcquireLimits {
            max_unpacked_bytes: 4096,
            ..Default::default()
        };
        rejects(&compressed, true, limits);
        let mut trailing = tar_bytes(&[("package/value", b'0', b"x")]);
        trailing.extend_from_slice(b"hidden data");
        rejects(&trailing, false, PackageAcquireLimits::default());
        let root = tempfile::tempdir().unwrap();
        extract(
            &tar_bytes(&[("package/script", b'0', b"code")]),
            false,
            root.path(),
            PackageAcquireLimits::default(),
            &TurnExecutionContext::new(),
            Instant::now(),
        )
        .unwrap();
        assert_eq!(
            fs::metadata(root.path().join("script"))
                .unwrap()
                .permissions()
                .mode()
                & 0o7777,
            0o700
        );
        let mut corrupt = compressed.clone();
        let last = corrupt.len() - 1;
        corrupt[last] ^= 1;
        rejects(&corrupt, true, PackageAcquireLimits::default());
        let deep = format!("package/{}/value", vec!["directory"; 65].join("/"));
        let gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        let mut builder = tar::Builder::new(gzip);
        let mut header = tar::Header::new_gnu();
        header.set_size(1);
        header.set_mode(0o600);
        header.set_cksum();
        builder
            .append_data(&mut header, deep, b"x".as_slice())
            .unwrap();
        rejects(
            &builder.into_inner().unwrap().finish().unwrap(),
            true,
            PackageAcquireLimits::default(),
        );
        let nodes = PackageAcquireLimits {
            max_entries: 2,
            ..Default::default()
        };
        rejects(
            &tar_bytes(&[("package/a/b/value", b'0', b"x")]),
            false,
            nodes,
        );
    }
    #[test]
    fn version_resolution_and_strongest_integrity_are_explicit() {
        for version in ["1.0.0", "0.2.1-alpha.1+build", "10.20.30"] {
            assert!(concrete_version(version));
        }
        for version in ["latest", "1.0", "01.0.0", "1.0.0-01", "1.0.0+"] {
            assert!(!concrete_version(version));
        }
        let weak = base64::engine::general_purpose::STANDARD
            .encode(ring::digest::digest(&ring::digest::SHA256, b"bytes").as_ref());
        let dist = NpmDist {
            tarball: "unused".into(),
            integrity: Some(format!("sha512-AQID sha256-{weak}")),
            shasum: None,
        };
        assert!(
            verify_integrity(&dist, b"bytes").is_err(),
            "cannot fall back from a mismatched stronger digest"
        );
    }
}

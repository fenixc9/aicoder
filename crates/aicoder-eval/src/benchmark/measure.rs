use super::*;
use std::{
    io::Read,
    process::{Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SourceStamp {
    pub revision: String,
    pub dirty: bool,
    pub source_sha256: String,
    pub file_count: usize,
    pub scope: String,
    pub executable_sha256: Option<String>,
    pub grader_rustc: Option<String>,
    pub platform: String,
}
impl SourceStamp {
    /// Read a trusted pre-launch source observation without mounting source into an executor.
    ///
    /// This is metadata, never restored execution authority. Validate the observed executable,
    /// platform and grading compiler against this process; the host owns the source claim.
    pub fn load_observation(path: impl AsRef<Path>) -> Result<Self> {
        let bytes = crate::bounds::read(path.as_ref(), 16 * 1024)?;
        let observed: Self = serde_json::from_slice(&bytes)
            .map_err(|_| anyhow::anyhow!("invalid benchmark source observation"))?;
        observed.validate()?;
        ensure!(
            observed.executable_sha256.as_deref() == Some(&executable_fingerprint()?)
                && observed.platform
                    == format!("{}/{}", std::env::consts::OS, std::env::consts::ARCH)
                && observed.grader_rustc == grader_compiler()?,
            "source observation does not match the current executable/platform/compiler"
        );
        Ok(observed)
    }
    pub(crate) fn validate(&self) -> Result<()> {
        ensure!(
            (40..=64).contains(&self.revision.len())
                && self.revision.bytes().all(|b| b.is_ascii_hexdigit())
                && self.source_sha256.len() == 64
                && self.source_sha256.bytes().all(|b| b.is_ascii_hexdigit())
                && (1..=20_000).contains(&self.file_count),
            "invalid source stamp"
        );
        ensure!(
            self.executable_sha256
                .as_ref()
                .is_none_or(|h| h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit()))
                && self
                    .grader_rustc
                    .as_ref()
                    .is_none_or(|v| v.len() <= 256 && !v.chars().any(char::is_control))
                && self.platform.len() <= 64
                && self.scope.len() <= 256,
            "invalid source observation metadata"
        );
        Ok(())
    }
    /// Hash tracked and untracked Rust/manifests/embedded assets without reading environment files.
    pub fn capture(root: impl AsRef<Path>) -> Result<Self> {
        let root = root.as_ref();
        let revision = String::from_utf8(git(root, &["rev-parse", "HEAD"])?)?
            .trim()
            .to_owned();
        ensure!(
            (40..=64).contains(&revision.len()) && revision.bytes().all(|b| b.is_ascii_hexdigit()),
            "invalid source revision"
        );
        let dirty = !git(root, &["status", "--porcelain", "--untracked-files=normal"])?.is_empty();
        let list = git(
            root,
            &[
                "ls-files",
                "-z",
                "--cached",
                "--others",
                "--exclude-standard",
                "--",
                "crates",
                "Cargo.toml",
                "Cargo.lock",
                "rust-toolchain.toml",
            ],
        )?;
        let mut paths = std::collections::BTreeSet::new();
        for path in list.split(|b| *b == 0).filter(|p| !p.is_empty()) {
            let path = std::str::from_utf8(path)?;
            if [
                ".rs",
                ".js",
                ".json",
                ".html",
                ".css",
                ".py",
                "Cargo.toml",
                "Cargo.lock",
                "rust-toolchain.toml",
            ]
            .iter()
            .any(|suffix| path.ends_with(suffix))
            {
                paths.insert(path.to_string());
            }
        }
        ensure!(
            !paths.is_empty() && paths.len() <= 20_000,
            "source inventory budget exceeded"
        );
        let mut digest = ring::digest::Context::new(&ring::digest::SHA256);
        let mut total = 0usize;
        for path in &paths {
            let bytes = crate::bounds::read(&root.join(path), 4 * 1024 * 1024)?;
            total = total
                .checked_add(bytes.len())
                .ok_or_else(|| anyhow::anyhow!("source byte count overflow"))?;
            ensure!(total <= 32 * 1024 * 1024, "source input budget exceeded");
            digest.update(path.as_bytes());
            digest.update(&[0]);
            digest.update(&bytes);
        }
        let grader_rustc = grader_compiler()?;
        let executable_sha256 = executable_fingerprint().ok();
        Ok(Self {
            revision, dirty, executable_sha256, grader_rustc,
            platform:format!("{}/{}",std::env::consts::OS,std::env::consts::ARCH),
            source_sha256:digest.finish().as_ref().iter().map(|b|format!("{b:02x}")).collect(),
            file_count:paths.len(),
            scope:"Rust, Cargo/toolchain manifests and embedded JS/JSON/HTML/CSS/Python; includes untracked sources".into(),
        })
    }
}
fn grader_compiler() -> Result<Option<String>> {
    let probe = tempfile::tempdir()?;
    std::fs::write(
        probe.path().join("rust-toolchain.toml"),
        include_str!("../../../../rust-toolchain.toml"),
    )?;
    Ok(metadata(probe.path(), "rustc", &["--version"])
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .map(|text| text.trim().to_string())
        .filter(|text| text.len() <= 256 && !text.chars().any(char::is_control)))
}
fn git(root: &Path, args: &[&str]) -> Result<Vec<u8>> {
    metadata(root, "git", args)
}
fn metadata(root: &Path, program: &str, args: &[&str]) -> Result<Vec<u8>> {
    let mut child = Command::new(program)
        .current_dir(root)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let mut bytes = Vec::new();
    child
        .stdout
        .take()
        .expect("piped Git")
        .take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 1024 * 1024 {
        let _ = child.kill();
        let _ = child.wait();
        anyhow::bail!("Git source metadata budget exhausted");
    }
    ensure!(child.wait()?.success(), "cannot read source Git metadata");
    Ok(bytes)
}
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MemoryObservation {
    pub scope: String,
    pub precision: String,
    pub sampled_peak_rss_kib: Option<u64>,
    pub samples: u64,
    pub includes_children: bool,
}
pub(crate) struct MemoryMeter {
    stop: Arc<AtomicBool>,
    data: Arc<Mutex<MemoryObservation>>,
    owner: Option<thread::JoinHandle<()>>,
}
impl MemoryMeter {
    pub fn start() -> Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let data = Arc::new(Mutex::new(MemoryObservation {
            scope: "benchmark_process_during_eval_execution_and_grading".into(),
            precision: "sampled_every_10ms_and_at_endpoints".into(),
            ..Default::default()
        }));
        sample(&data);
        let observed = data.clone();
        let stopping = stop.clone();
        let owner = thread::Builder::new()
            .name("aicoder-bench-rss".into())
            .spawn(move || {
                while !stopping.load(Ordering::Acquire) {
                    sample(&observed);
                    thread::park_timeout(Duration::from_millis(10));
                }
                sample(&observed);
            })?;
        Ok(Self {
            stop,
            data,
            owner: Some(owner),
        })
    }
    pub fn finish(mut self) -> MemoryObservation {
        self.close();
        self.data.lock().expect("RSS meter poisoned").clone()
    }
    fn close(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(owner) = self.owner.take() {
            owner.thread().unpark();
            let _ = owner.join();
        }
    }
}
impl Drop for MemoryMeter {
    fn drop(&mut self) {
        self.close();
    }
}
fn sample(data: &Mutex<MemoryObservation>) {
    #[cfg(target_os = "linux")]
    if let Ok(mut file) = std::fs::File::open("/proc/self/status") {
        let mut bytes = Vec::new();
        if file
            .by_ref()
            .take(16 * 1024)
            .read_to_end(&mut bytes)
            .is_ok()
            && let Some(kib) = String::from_utf8_lossy(&bytes).lines().find_map(|line| {
                line.strip_prefix("VmRSS:")
                    .and_then(|s| s.split_whitespace().next()?.parse::<u64>().ok())
            })
        {
            let mut data = data.lock().expect("RSS meter poisoned");
            data.sampled_peak_rss_kib = Some(data.sampled_peak_rss_kib.unwrap_or(0).max(kib));
            data.samples = data.samples.saturating_add(1);
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = data;
    }
}

pub fn executable_fingerprint() -> Result<String> {
    let mut file = std::fs::File::open("/proc/self/exe")?;
    ensure!(
        file.metadata()?.len() <= 512 * 1024 * 1024,
        "benchmark executable exceeds fingerprint budget"
    );
    let mut digest = ring::digest::Context::new(&ring::digest::SHA256);
    let mut buffer = [0u8; 65536];
    let mut bytes = 0usize;
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        bytes += n;
        ensure!(
            bytes <= 512 * 1024 * 1024,
            "executable grew beyond fingerprint budget"
        );
        digest.update(&buffer[..n]);
    }
    Ok(digest
        .finish()
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

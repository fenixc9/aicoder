//! Complete private staging and no-clobber publication for a pinned fork source.
use super::*;
use super::{jsonl::*, limits::check};
use aicoder_core::TurnExecutionContext;
use anyhow::{Context, Result, ensure};
use fs2::FileExt;
use std::{
    fs::{self, File},
    io::Write,
    ops::Range,
    path::PathBuf,
    sync::Mutex,
};
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct Stamp {
    dev: u64,
    ino: u64,
    len: u64,
    mtime: i64,
    mtime_ns: i64,
    ctime: i64,
    ctime_ns: i64,
}
impl Stamp {
    pub fn new(m: &fs::Metadata) -> Self {
        use std::os::unix::fs::MetadataExt;
        Self {
            dev: m.dev(),
            ino: m.ino(),
            len: m.len(),
            mtime: m.mtime(),
            mtime_ns: m.mtime_nsec(),
            ctime: m.ctime(),
            ctime_ns: m.ctime_nsec(),
        }
    }
}
pub(super) struct ForkSourcePin {
    pub root: PathBuf,
    pub path: PathBuf,
    pub file: Mutex<File>,
    pub stamp: Stamp,
    pub expected_len: u64,
    pub hash: u64,
    pub max_scan: u64,
    pub ranges: Vec<Range<usize>>,
    pub output_bytes: usize,
}
pub(super) fn verify(pin: &ForkSourcePin, context: &TurnExecutionContext) -> Result<()> {
    if context.is_cancelled() {
        return Err(context.error().into());
    }
    let mut file = pin
        .file
        .lock()
        .map_err(|_| anyhow::anyhow!("fork source descriptor poisoned"))?;
    let (len, hash, _) = super::archive::hash_file(&mut file, None, pin.max_scan)?;
    let named = fs::symlink_metadata(&pin.path)?;
    ensure!(
        !named.file_type().is_symlink()
            && len == pin.expected_len
            && hash == pin.hash
            && Stamp::new(&file.metadata()?) == pin.stamp
            && Stamp::new(&named) == pin.stamp,
        "fork source changed"
    );
    if context.is_cancelled() {
        return Err(context.error().into());
    }
    Ok(())
}
/// Bounds include each encoded record and complete file framing, before any stage file exists.
pub(super) fn layout(
    plan: &SessionForkPlan,
    max_record: usize,
) -> Result<(Vec<Range<usize>>, usize)> {
    let entries = plan.snapshot().entries();
    let mut ranges = Vec::new();
    let mut start = 0;
    let mut estimated = 0usize;
    for (i, entry) in entries.iter().enumerate() {
        let bytes = bounded_size(entry, max_record)?;
        let framing = bounded_size(&Some(entry.id.clone()), max_record)?.saturating_add(512);
        ensure!(
            bytes.saturating_add(framing) <= max_record,
            "one fork entry exceeds output record budget"
        );
        if i > start && estimated.saturating_add(bytes).saturating_add(framing) > max_record {
            ranges.push(start..i);
            start = i;
            estimated = 0;
        }
        estimated = estimated.saturating_add(bytes).saturating_add(1);
    }
    if start < entries.len() {
        ranges.push(start..entries.len());
    }
    let mut size = header(plan.target())?.len();
    for (i, range) in ranges.iter().enumerate() {
        let block = &entries[range.clone()];
        let bytes = transaction_line(
            i as u64 + 1,
            plan.target().updated_at,
            block,
            &block.last().map(|e| e.id.clone()),
        )?;
        check("fork record bytes", bytes.len() as u64, max_record as u64)?;
        size = size
            .checked_add(bytes.len())
            .context("fork output size overflow")?;
        check(
            "fork complete output",
            size as u64,
            plan.limits().max_bytes as u64,
        )?;
    }
    Ok((ranges, size))
}
pub(super) fn bounded_size(value: &impl serde::Serialize, limit: usize) -> Result<usize> {
    struct Bound {
        used: usize,
        limit: usize,
    }
    impl Write for Bound {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            if b.len() > self.limit.saturating_sub(self.used) {
                return Err(std::io::Error::other("fork serialization budget exceeded"));
            }
            self.used += b.len();
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut out = Bound { used: 0, limit };
    serde_json::to_writer(&mut out, value)?;
    Ok(out.used)
}
struct Staged(PathBuf);
impl Drop for Staged {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}
pub(super) fn publish(
    repository: &JsonlSessionRepository,
    source: &RunSession,
    plan: SessionForkPlan,
    context: &TurnExecutionContext,
) -> Result<PublishedSessionFork> {
    plan.validate_source(source)?;
    plan.check_context(context)?;
    let pin = plan
        .backend::<ForkSourcePin>()
        .context("fork has no native source pin")?;
    ensure!(
        pin.root == repository.root(),
        "fork belongs to another repository"
    );
    let _source_meta = repository.metadata_lock(&plan.source().id, true)?;
    let source_token = match source {
        RunSession::Full(s) => s.run_token(),
        RunSession::Window(w) => w.run_token(),
    };
    ensure!(
        source_token.is_some(),
        "fork requires an owned source lease"
    );
    let _source_owner = repository.write_lease(&plan.source().id, source_token)?;
    verify(pin, context)?;
    let id = plan.target().id.clone();
    let _target_meta = repository.metadata_lock(&id, true)?;
    let mut run = repository.run_file(&id)?;
    FileExt::try_lock_exclusive(&run).context("fork target is already owned")?;
    let token = uuid::Uuid::new_v4().to_string();
    run.set_len(0)?;
    run.write_all(token.as_bytes())?;
    run.flush()?;
    let target = repository.root().join(format!("{id}.jsonl"));
    ensure!(!target.try_exists()?, "fork target already exists");
    let staged = Staged(
        repository
            .root()
            .join(format!(".fork-{}.tmp", uuid::Uuid::new_v4())),
    );
    let mut file = create_private_readwrite(&staged.0)?;
    file.write_all(&header(plan.target())?)?;
    for (i, range) in pin.ranges.iter().enumerate() {
        plan.check_context(context)?;
        let block = &plan.snapshot().entries()[range.clone()];
        let bytes = transaction_line(
            i as u64 + 1,
            plan.target().updated_at,
            block,
            &block.last().map(|e| e.id.clone()),
        )?;
        check(
            "fork record bytes",
            bytes.len() as u64,
            repository.limits.max_record_bytes as u64,
        )?;
        file.write_all(&bytes)?;
    }
    file.sync_all()?;
    ensure!(
        file.metadata()?.len() == pin.output_bytes as u64,
        "fork staging length mismatch"
    );
    let (length, hash, _) =
        super::archive::hash_file(&mut file, None, plan.limits().max_bytes as u64)?;
    let staged_stamp = Stamp::new(&file.metadata()?);
    ensure!(
        Stamp::new(&fs::symlink_metadata(&staged.0)?) == staged_stamp,
        "fork staging identity changed"
    );
    verify(pin, context)?;
    plan.check_context(context)?;
    fs::hard_link(&staged.0, &target)?;
    use std::os::unix::fs::MetadataExt;
    let named = fs::symlink_metadata(&target).map_err(|_| SessionForkPublishFailure {
        visible: Some(plan.target().clone()),
        stage: "published identity",
    })?;
    if named.file_type().is_symlink()
        || named.dev() != staged_stamp.dev
        || named.ino() != staged_stamp.ino
    {
        return Err(SessionForkPublishFailure {
            visible: Some(plan.target().clone()),
            stage: "published identity",
        }
        .into());
    }
    if sync_directory(repository.root()).is_err() {
        return Err(SessionForkPublishFailure {
            visible: Some(plan.target().clone()),
            stage: "directory durability",
        }
        .into());
    }
    let session = if let Some(preview) = plan.window_preview() {
        let mut window = preview.clone();
        super::archive::bind_fork_window(&mut window, target, length, hash);
        window.attach_run_token(Some(token));
        RunSession::Window(Box::new(window))
    } else {
        let mut result = plan.snapshot().clone();
        result.clear_backend_state();
        result.attach_backend_state(
            StorageCursor {
                path: target.clone(),
                expected_len: length,
                valid_len: length,
                fingerprint: hash,
                needs_separator: false,
            },
            Some(target),
        );
        result.attach_run_token(Some(token));
        RunSession::Full(Box::new(result))
    };
    Ok(PublishedSessionFork {
        session,
        lease: Box::new(Lock(run)),
    })
}

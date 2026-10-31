use aicoder_core::{artifacts::*, tools::*};
use aicoder_runtime::{
    artifacts::FileArtifactStore,
    tools::{BashTool, ShellLimits, ShellOutput},
};
use serde_json::json;
use std::{sync::Arc, time::Duration};
fn archived_shell() -> aicoder_runtime::tools::ConfiguredBash {
    BashTool::with_limits(ShellLimits::default())
        .unwrap()
        .with_archive_timeout(Duration::from_secs(2))
        .unwrap()
}
fn store(root: &std::path::Path, workspace: &std::path::Path) -> Arc<FileArtifactStore> {
    Arc::new(
        FileArtifactStore::new(
            root,
            workspace,
            ArtifactLimits {
                max_artifact_bytes: 128 * 1024,
                max_total_bytes: 1024 * 1024,
                max_artifacts: 16,
            },
        )
        .unwrap()
        .with_lock_timeout(Duration::from_secs(1))
        .unwrap(),
    )
}
async fn collect(store: &dyn ArtifactStore, id: &str) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut offset = 0;
    loop {
        let page = store.read(id, offset, 777).await.unwrap();
        bytes.extend(page.bytes);
        if let Some(next) = page.next_offset {
            offset = next;
        } else {
            return bytes;
        }
    }
}
#[tokio::test]
async fn disk_artifacts_reopen_exactly_and_reject_foreign_scopes_paths_and_symlinks() {
    let storage = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let other = tempfile::tempdir().unwrap();
    let root = storage.path().join("private");
    let store = store(&root, workspace.path());
    assert!(!root.exists());
    let bytes = (0..9999).map(|i| (i % 256) as u8).collect::<Vec<_>>();
    let reference = store_bytes(store.as_ref(), "application/octet-stream", &bytes, true)
        .await
        .unwrap();
    drop(store);
    let restored = FileArtifactStore::new(&root, workspace.path(), Default::default()).unwrap();
    assert_eq!(collect(&restored, &reference.id).await, bytes);
    let foreign = FileArtifactStore::new(&root, other.path(), Default::default()).unwrap();
    assert_eq!(
        foreign.read(&reference.id, 0, 1).await.unwrap_err().code,
        "scope_mismatch"
    );
    assert!(foreign.remove(&reference.id).await.is_err());
    assert!(restored.read("../private", 0, 1).await.is_err());
    #[cfg(unix)]
    {
        use std::os::unix::fs::{PermissionsExt, symlink};
        assert_eq!(
            std::fs::metadata(&root).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(root.join(format!("{}.artifact", reference.id)))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        let id = "00000000-0000-0000-0000-000000000001";
        symlink(
            root.join(format!("{}.artifact", reference.id)),
            root.join(format!("{id}.artifact")),
        )
        .unwrap();
        assert!(restored.read(id, 0, 1).await.is_err());
    }
}
#[tokio::test]
async fn concurrent_reservations_are_bounded_and_live_writers_are_not_reaped() {
    let storage = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let root = storage.path().join("store");
    let make = || {
        FileArtifactStore::new(
            &root,
            workspace.path(),
            ArtifactLimits {
                max_artifact_bytes: 4096,
                max_total_bytes: 4096,
                max_artifacts: 1,
            },
        )
        .unwrap()
    };
    let a = make();
    let b = make();
    let writer = a.begin("text/plain").await.unwrap();
    assert!(b.begin("text/plain").await.is_err());
    assert_eq!(b.reap_incomplete(Duration::ZERO).await.unwrap(), 0);
    drop(writer);
    let writer = b.begin("text/plain").await.unwrap();
    drop(writer);
    assert!(std::fs::read_dir(&root).unwrap().all(|entry| {
        entry
            .unwrap()
            .path()
            .extension()
            .is_none_or(|s| s != "part")
    }));
    let stale = root.join("00000000-0000-0000-0000-000000000001.part");
    std::fs::write(&stale, b"orphan").unwrap();
    assert_eq!(b.reap_incomplete(Duration::ZERO).await.unwrap(), 1);
}
#[tokio::test]
async fn shell_archive_keeps_hidden_middle_bytes_and_small_outputs_create_no_files() {
    let storage = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let store = store(storage.path(), workspace.path());
    let context = ToolContext::new(workspace.path(), Duration::from_secs(5), 4096)
        .unwrap()
        .with_artifact_store(store.clone());
    let small = BashTool
        .execute(&context, json!({"command":"printf small"}))
        .await
        .unwrap();
    assert!(small.artifacts.is_empty());
    assert!(!storage.path().join(".lock").exists());
    let output=archived_shell().execute(&context,json!({"command":"printf 'head\\n'; printf '%050000d' 0; printf '\\000\\377MIDDLE'; printf '%050000d' 0; printf 'tail\\n'"})).await.unwrap();
    let details: ShellOutput = serde_json::from_value(output.output.clone()).unwrap();
    assert!(details.truncated && details.output_complete);
    assert!(output.output.to_string().len() <= 4096);
    assert!(!details.stdout.contains("MIDDLE"));
    let reference = details
        .stdout_artifact
        .clone()
        .unwrap_or_else(|| panic!("Missing stdout archive: {details:?}"));
    assert!(reference.complete);
    assert_eq!(
        output.artifacts.as_slice(),
        std::slice::from_ref(&reference)
    );
    let bytes = collect(store.as_ref(), &reference.id).await;
    assert_eq!(bytes.len() as u64, details.stdout_bytes);
    assert!(bytes.windows(8).any(|s| s == b"\0\xffMIDDLE"));
    assert!(bytes.starts_with(b"head\n") && bytes.ends_with(b"tail\n"));
}
#[tokio::test]
async fn archive_cap_is_partial_without_stopping_a_successful_command() {
    let storage = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let store = Arc::new(
        FileArtifactStore::new(
            storage.path(),
            workspace.path(),
            ArtifactLimits {
                max_artifact_bytes: 2048,
                max_total_bytes: 16384,
                max_artifacts: 4,
            },
        )
        .unwrap(),
    );
    let context = ToolContext::new(workspace.path(), Duration::from_secs(5), 1024)
        .unwrap()
        .with_artifact_store(store.clone());
    let success = archived_shell()
        .execute(
            &context,
            json!({"command":"printf '%010000d' 0; printf FINAL"}),
        )
        .await
        .unwrap();
    let output: ShellOutput = serde_json::from_value(success.output).unwrap();
    assert_eq!(output.exit_code, Some(0));
    assert!(output.output_complete);
    let reference = output
        .stdout_artifact
        .clone()
        .unwrap_or_else(|| panic!("Missing stdout archive: {output:?}"));
    assert!(!reference.complete);
    assert_eq!(reference.bytes, 2048);
    assert_eq!(
        collect(store.as_ref(), &reference.id).await,
        vec![b'0'; 2048]
    );
}
#[tokio::test]
async fn shell_failures_keep_captured_artifacts_and_quota_errors_do_not_rerun_commands() {
    let storage = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let store = store(storage.path(), workspace.path());
    let context = ToolContext::new(workspace.path(), Duration::from_secs(5), 2048)
        .unwrap()
        .with_artifact_store(store.clone());
    let error = archived_shell()
        .execute(
            &context,
            json!({"command":"printf '%050000d' 0 >&2; exit 7"}),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, "command_failed");
    let output: ShellOutput = serde_json::from_value(error.details.unwrap()).unwrap();
    let reference = output
        .stderr_artifact
        .clone()
        .unwrap_or_else(|| panic!("Missing stderr archive: {output:?}"));
    assert!(reference.complete);
    assert_eq!(error.artifacts.as_slice(), std::slice::from_ref(&reference));
    assert_eq!(collect(store.as_ref(), &reference.id).await.len(), 50000);
    let limited = Arc::new(
        FileArtifactStore::new(
            storage.path().join("quota"),
            workspace.path(),
            ArtifactLimits {
                max_artifact_bytes: 100,
                max_total_bytes: 100,
                max_artifacts: 1,
            },
        )
        .unwrap(),
    );
    let pending = limited.begin("text/plain").await.unwrap();
    let context = context.with_artifact_store(limited);
    let output = BashTool
        .execute(
            &context,
            json!({"command":"printf once >> calls; printf '%010000d' 0"}),
        )
        .await
        .unwrap();
    assert_eq!(
        std::fs::read(workspace.path().join("calls")).unwrap(),
        b"once"
    );
    assert_eq!(output.output["stdout_artifact_error"], "quota_exceeded");
    assert!(output.artifacts.is_empty());
    drop(pending);
}
#[tokio::test]
async fn scan_limit_archive_is_never_advertised_as_complete() {
    let storage = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let store = store(storage.path(), workspace.path());
    let context = ToolContext::new(workspace.path(), Duration::from_secs(5), 2048)
        .unwrap()
        .with_artifact_store(store.clone());
    let shell = BashTool::with_limits(ShellLimits {
        max_output_bytes: 6000,
        ..Default::default()
    })
    .unwrap()
    .with_archive_timeout(Duration::from_secs(2))
    .unwrap();
    let error = shell
        .execute(&context, json!({"command":"printf '%030000d' 0"}))
        .await
        .unwrap_err();
    assert_eq!(error.code, "output_limit");
    let output: ShellOutput = serde_json::from_value(error.details.unwrap()).unwrap();
    let reference = output
        .stdout_artifact
        .clone()
        .unwrap_or_else(|| panic!("Missing stdout archive: {output:?}"));
    assert!(!reference.complete);
    assert_eq!(collect(store.as_ref(), &reference.id).await.len(), 6001);
}

#[tokio::test]
async fn dropping_shell_with_live_archive_discards_staging_without_publishing_partial_facts() {
    let storage = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let root = storage.path().to_path_buf();
    let store = store(&root, workspace.path());
    let context = ToolContext::new(workspace.path(), Duration::from_secs(30), 2048)
        .unwrap()
        .with_artifact_store(store);
    let task = tokio::spawn(async move {
        BashTool
            .execute(
                &context,
                json!({"command":"printf '%050000d' 0; printf ready > ready; sleep 20"}),
            )
            .await
    });
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let staged = std::fs::read_dir(&root)
                .unwrap()
                .any(|e| e.unwrap().path().extension().is_some_and(|s| s == "part"));
            if staged && workspace.path().join("ready").exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if std::fs::read_dir(&root)
                .unwrap()
                .all(|e| e.unwrap().path().extension().is_none_or(|s| s != "part"))
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert!(std::fs::read_dir(&root).unwrap().all(|e| {
        e.unwrap()
            .path()
            .extension()
            .is_none_or(|s| s != "artifact")
    }));
}
#[tokio::test]
async fn corrupt_footer_lengths_are_rejected_before_payload_allocation() {
    use std::io::{Seek, SeekFrom, Write};
    let storage = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let store = store(storage.path(), workspace.path());
    let reference = store_bytes(store.as_ref(), "text/plain", b"original", true)
        .await
        .unwrap();
    let path = storage.path().join(format!("{}.artifact", reference.id));
    let mut file = std::fs::OpenOptions::new().write(true).open(path).unwrap();
    file.seek(SeekFrom::End(-8)).unwrap();
    file.write_all(&u64::MAX.to_le_bytes()).unwrap();
    drop(file);
    assert_eq!(
        store.read(&reference.id, 0, 1).await.unwrap_err().code,
        "corrupt"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_writers_publish_independent_immutable_artifacts() {
    let storage = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let store = store(storage.path(), workspace.path());
    let mut tasks = Vec::new();
    for index in 0..8 {
        let store = store.clone();
        tasks.push(tokio::spawn(async move {
            let bytes = vec![index; 4096];
            let reference = store_bytes(store.as_ref(), "application/octet-stream", &bytes, true)
                .await
                .unwrap();
            assert_eq!(collect(store.as_ref(), &reference.id).await, bytes);
            reference
        }));
    }
    let mut ids = std::collections::HashSet::new();
    for task in tasks {
        assert!(ids.insert(task.await.unwrap().id));
    }
    assert_eq!(ids.len(), 8);
}

#[test]
fn host_archive_and_lock_deadlines_are_bounded_and_cannot_be_disabled() {
    let root = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    for invalid in [Duration::ZERO, Duration::from_secs(6)] {
        assert!(
            BashTool::with_limits(ShellLimits::default())
                .unwrap()
                .with_archive_timeout(invalid)
                .is_err()
        );
        assert!(
            FileArtifactStore::new(root.path(), workspace.path(), ArtifactLimits::default())
                .unwrap()
                .with_lock_timeout(invalid)
                .is_err()
        );
    }
    assert!(!root.path().join(".lock").exists());
}
struct DelayedFinishStore(Arc<FileArtifactStore>);
struct DelayedFinishWriter(Box<dyn ArtifactWriter>);
#[async_trait::async_trait]
impl ArtifactStore for DelayedFinishStore {
    fn limits(&self) -> ArtifactLimits {
        self.0.limits()
    }
    async fn begin(&self, kind: &str) -> Result<Box<dyn ArtifactWriter>, ArtifactError> {
        Ok(Box::new(DelayedFinishWriter(self.0.begin(kind).await?)))
    }
    async fn read(
        &self,
        id: &str,
        offset: u64,
        limit: usize,
    ) -> Result<ArtifactPage, ArtifactError> {
        self.0.read(id, offset, limit).await
    }
    async fn remove(&self, id: &str) -> Result<(), ArtifactError> {
        self.0.remove(id).await
    }
}
#[async_trait::async_trait]
impl ArtifactWriter for DelayedFinishWriter {
    async fn write(&mut self, bytes: &[u8]) -> Result<(), ArtifactError> {
        self.0.write(bytes).await
    }
    async fn finish(self: Box<Self>, complete: bool) -> Result<ArtifactRef, ArtifactError> {
        tokio::time::sleep(Duration::from_millis(350)).await;
        self.0.finish(complete).await
    }
}
#[tokio::test]
async fn configured_archive_deadline_handles_slow_finalization_without_repeating_commands() {
    let storage = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let disk = store(storage.path(), workspace.path());
    let context = ToolContext::new(workspace.path(), Duration::from_secs(5), 2048)
        .unwrap()
        .with_artifact_store(Arc::new(DelayedFinishStore(disk.clone())));
    let command = "printf hit >> count; printf '%010000d' 0";
    let short = BashTool
        .execute(&context, json!({"command":command}))
        .await
        .unwrap();
    let output: ShellOutput = serde_json::from_value(short.output).unwrap();
    assert_eq!(
        output.stdout_artifact_error.as_deref(),
        Some("storage_timeout")
    );
    assert!(output.stdout_artifact.is_none());
    assert_eq!(
        std::fs::read_to_string(workspace.path().join("count")).unwrap(),
        "hit"
    );
    let long = archived_shell()
        .execute(&context, json!({"command":command}))
        .await
        .unwrap();
    let output: ShellOutput = serde_json::from_value(long.output).unwrap();
    let reference = output.stdout_artifact.unwrap();
    assert!(reference.complete);
    assert_eq!(reference.bytes, 10000);
    assert_eq!(collect(disk.as_ref(), &reference.id).await.len(), 10000);
    assert_eq!(
        std::fs::read_to_string(workspace.path().join("count")).unwrap(),
        "hithit"
    );
    assert!(!std::fs::read_dir(storage.path()).unwrap().any(|entry| {
        entry
            .unwrap()
            .path()
            .extension()
            .is_some_and(|extension| extension == "part")
    }));
}

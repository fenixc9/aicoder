use aicoder_core::{TurnExecutionContext, artifacts::*, tools::ToolContext};
use aicoder_runtime::{process::*, tools::ShellStopReason};
use std::{process::Command, sync::Arc, time::Duration};
fn context(root: &std::path::Path, time: Duration) -> ToolContext {
    ToolContext::new(root, time, 4096).unwrap()
}
#[tokio::test]
async fn native_arguments_are_literal_and_small_binary_output_is_archived_in_full() {
    let root = tempfile::tempdir().unwrap();
    let store = Arc::new(MemoryArtifactStore::default());
    let context = context(root.path(), Duration::from_secs(2)).with_artifact_store(store.clone());
    let mut command = Command::new("/usr/bin/printf");
    command.args(["\\377\\000%s", "$(touch bad)"]);
    let output = capture(
        command,
        &context,
        ProcessCaptureOptions::default(),
        TurnExecutionContext::new(),
    )
    .await
    .unwrap();
    assert_eq!(output.exit_code, Some(0));
    assert!(output.reaped && output.cleanup_ok && output.output_complete);
    assert!(!root.path().join("bad").exists());
    let reference = output.stdout_artifact.unwrap();
    assert!(reference.complete);
    let page = store.read(&reference.id, 0, 4096).await.unwrap();
    assert_eq!(page.bytes, b"\xff\0$(touch bad)");
    assert_eq!(reference.bytes, output.stdout_bytes);
}
#[tokio::test]
async fn deadlines_and_explicit_cancellation_await_the_same_group_cleanup() {
    for cancel in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let context = context(
            root.path(),
            if cancel {
                Duration::from_secs(5)
            } else {
                Duration::from_millis(200)
            },
        );
        let scope = TurnExecutionContext::new();
        let worker = scope.clone();
        let mut command = Command::new("/bin/bash");
        command.args([
            "--noprofile",
            "--norc",
            "-c",
            "sleep 30 & printf %s $! > child.pid; printf running; wait",
        ]);
        let task = tokio::spawn(async move {
            capture(command, &context, ProcessCaptureOptions::default(), worker)
                .await
                .unwrap()
        });
        tokio::time::timeout(Duration::from_secs(2), async {
            while !root.path().join("child.pid").exists() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        if cancel {
            scope.cancel("test host cancellation");
        }
        let output = tokio::time::timeout(Duration::from_secs(3), task)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            output.stop_reason,
            if cancel {
                ShellStopReason::Cancelled
            } else {
                ShellStopReason::Timeout
            }
        );
        assert!(
            output.process_group_signal_requested && output.reaped && output.cleanup_ok,
            "{output:?}"
        );
        let pid = std::fs::read_to_string(root.path().join("child.pid")).unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let stat = std::fs::read_to_string(format!("/proc/{pid}/stat"));
                if stat.as_ref().map_or(true, |text| {
                    text.rsplit_once(')')
                        .is_some_and(|(_, tail)| tail.trim_start().starts_with('Z'))
                }) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
    }
}
#[tokio::test]
async fn dropped_capture_signals_cleanup_without_waiting_for_a_tokio_cleanup_task() {
    let root = tempfile::tempdir().unwrap();
    let context = context(root.path(), Duration::from_secs(30));
    let mut command = Command::new("/bin/bash");
    command.args([
        "--noprofile",
        "--norc",
        "-c",
        "sleep 30 & printf %s $! > child.pid; wait",
    ]);
    let owner = tokio::spawn(async move {
        capture(
            command,
            &context,
            ProcessCaptureOptions::default(),
            TurnExecutionContext::new(),
        )
        .await
    });
    tokio::time::timeout(Duration::from_secs(2), async {
        while !root.path().join("child.pid").exists() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    owner.abort();
    let _ = owner.await;
    let pid = std::fs::read_to_string(root.path().join("child.pid")).unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let stat = std::fs::read_to_string(format!("/proc/{pid}/stat"));
            if stat.as_ref().map_or(true, |text| {
                text.rsplit_once(')')
                    .is_some_and(|(_, tail)| tail.trim_start().starts_with('Z'))
            }) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}
#[tokio::test]
async fn pre_cancelled_capture_never_spawns_and_output_scan_limits_remain_distinct() {
    let root = tempfile::tempdir().unwrap();
    let context = context(root.path(), Duration::from_secs(2));
    let cancellation = TurnExecutionContext::new();
    cancellation.cancel("before admission");
    let mut command = Command::new("touch");
    command.arg("never");
    let output = capture(
        command,
        &context,
        ProcessCaptureOptions::default(),
        cancellation,
    )
    .await
    .unwrap();
    assert_eq!(output.stop_reason, ShellStopReason::Cancelled);
    assert!(!root.path().join("never").exists());
    assert!(!output.process_group_signal_requested);
    let mut options = ProcessCaptureOptions::default();
    options.limits.max_output_bytes = 1024;
    let mut command = Command::new("/bin/bash");
    command.args(["-c", "while :; do printf 'large output chunk'; done"]);
    let output = capture(command, &context, options, TurnExecutionContext::new())
        .await
        .unwrap();
    assert_eq!(output.stop_reason, ShellStopReason::OutputLimit);
    assert!(output.reaped && output.cleanup_ok);
    assert!(output.stdout_bytes <= 1025);
    assert!(!output.output_complete);
}

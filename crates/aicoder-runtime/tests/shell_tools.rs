#![cfg(target_os = "linux")]
use aicoder_core::{model::*, tools::*, *};
use aicoder_runtime::tools::{
    BashTool, ShellLimits, ShellOutput, ShellStopReason, default_registry,
};
use async_trait::async_trait;
use serde_json::json;
use std::{fs, path::Path, sync::Arc, time::Duration};
fn context(root: &Path, bytes: usize, duration: Duration) -> ToolContext {
    ToolContext::new(root, duration, bytes).unwrap()
}
async fn pid(path: &Path) -> u32 {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if let Ok(text) = fs::read_to_string(path)
                && let Ok(pid) = text.trim().parse()
            {
                break pid;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap()
}
async fn reaped(pid: u32) {
    tokio::time::timeout(Duration::from_secs(3), async {
        while Path::new(&format!("/proc/{pid}")).exists() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("owned shell leader must be reaped");
}
#[tokio::test]
async fn long_output_keeps_both_stream_heads_and_tails_without_stopping_a_valid_command() {
    let root = tempfile::tempdir().unwrap();
    let ctx = context(root.path(), 4096, Duration::from_secs(5));
    let script = "printf OUTHEAD; printf ERRHEAD >&2; for ((i=0;i<256;i++)); do printf '%4096s' X; printf '%4096s' Y >&2; done; printf OUTTAIL; printf ERRTAIL >&2; : > completed";
    let result = BashTool
        .execute(&ctx, json!({"command":script}))
        .await
        .unwrap();
    let output = result.output;
    assert_eq!(output["exit_code"], 0);
    assert_eq!(output["stop_reason"], "exit");
    assert_eq!(output["output_complete"], true);
    assert_eq!(output["reaped"], true);
    assert!(result.truncated);
    assert!(root.path().join("completed").exists());
    assert!(output.to_string().len() <= 4096);
    for stream in ["stdout", "stderr"] {
        let text = output[stream].as_str().unwrap();
        assert!(text.starts_with(if stream == "stdout" {
            "OUTHEAD"
        } else {
            "ERRHEAD"
        }));
        assert!(text.ends_with(if stream == "stdout" {
            "OUTTAIL"
        } else {
            "ERRTAIL"
        }));
        assert_eq!(output[format!("{stream}_bytes")], 1024 * 1024 + 14);
        assert!(output[format!("{stream}_omitted_bytes")].as_u64().unwrap() > 0);
    }
}
#[tokio::test]
async fn valid_utf8_remains_valid_when_retention_and_json_escaping_cut_streams() {
    let root = tempfile::tempdir().unwrap();
    let ctx = context(root.path(), 2048, Duration::from_secs(3));
    let result = BashTool
        .execute(
            &ctx,
            json!({"command":"for ((i=0;i<5000;i++)); do printf '\"\\\\中文🦀'; done"}),
        )
        .await
        .unwrap();
    assert!(result.output.to_string().len() <= 2048);
    let text = result.output["stdout"].as_str().unwrap();
    assert!(!text.contains('\u{fffd}'));
    assert!(result.truncated);
    assert_eq!(result.output["output_complete"], true);
}
#[tokio::test]
async fn binary_output_and_signal_exit_are_explicit_structured_results() {
    let root = tempfile::tempdir().unwrap();
    let ctx = context(root.path(), 4096, Duration::from_secs(3));
    let error = BashTool
        .execute(
            &ctx,
            json!({"command":"printf '\\377\\000ok'; printf diagnostic >&2; kill -TERM $$"}),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, "command_failed");
    let details = error.details.unwrap();
    assert!(details["exit_code"].is_null());
    assert_eq!(details["signal"], libc::SIGTERM);
    assert_eq!(details["stderr"], "diagnostic");
    assert_eq!(details["encoding"], "utf8_lossy");
    assert!(details["stdout"].as_str().unwrap().contains('\u{fffd}'));
    assert_eq!(details["reaped"], true);
    assert!(details.to_string().len() <= 4096);
    let typed: ShellOutput = serde_json::from_value(details).unwrap();
    assert_eq!(typed.stop_reason, ShellStopReason::Exit);
    assert_eq!(typed.signal, Some(libc::SIGTERM));
}
#[tokio::test]
async fn combined_output_scan_limit_terminates_the_producer_and_reaps_its_leader() {
    let root = tempfile::tempdir().unwrap();
    let ctx = context(root.path(), 2048, Duration::from_secs(5));
    let tool = BashTool::with_limits(ShellLimits {
        max_output_bytes: 32768,
        ..Default::default()
    })
    .unwrap();
    let error=tool.execute(&ctx,json!({"command":"printf '%s' $$ > leader; (sleep 0.5; : > late) & while :; do printf '%1024s' X; printf '%1024s' Y >&2; done"})).await.unwrap_err();
    assert_eq!(error.code, "output_limit");
    let output = error.details.unwrap();
    assert_eq!(output["stop_reason"], "output_limit");
    assert_eq!(
        output["stdout_bytes"].as_u64().unwrap() + output["stderr_bytes"].as_u64().unwrap(),
        32769
    );
    assert_eq!(output["output_complete"], false);
    assert_eq!(output["reaped"], true);
    assert!(output.to_string().len() <= 2048);
    reaped(pid(&root.path().join("leader")).await).await;
    tokio::time::sleep(Duration::from_millis(650)).await;
    assert!(!root.path().join("late").exists());
}
#[tokio::test]
async fn explicit_command_timeout_returns_partial_output_after_cleanup() {
    let root = tempfile::tempdir().unwrap();
    let ctx = context(root.path(), 4096, Duration::from_secs(5));
    let error=BashTool.execute(&ctx,json!({"command":"printf partial; printf '%s' $$ > leader; sleep 30","timeout_seconds":1})).await.unwrap_err();
    assert_eq!(error.code, "timeout");
    let output = error.details.unwrap();
    assert_eq!(output["stdout"], "partial");
    assert_eq!(output["stop_reason"], "timeout");
    assert_eq!(output["reaped"], true);
    assert_eq!(output["cleanup_ok"], true);
    reaped(pid(&root.path().join("leader")).await).await;
}
#[tokio::test]
async fn host_deadline_retains_partial_output_and_dropped_tools_cannot_leave_a_live_group() {
    let root = tempfile::tempdir().unwrap();
    let ctx = context(root.path(), 4096, Duration::from_millis(120));
    let error=BashTool.execute(&ctx,json!({"command":"printf partial; printf '%s' $$ > leader; (sleep 0.5; : > late) & wait"})).await.unwrap_err();
    assert_eq!(error.code, "timeout");
    let output = error.details.unwrap();
    assert_eq!(output["stdout"], "partial");
    assert_eq!(output["stop_reason"], "tool_timeout");
    assert_eq!(output["process_group_signal_requested"], true);
    reaped(pid(&root.path().join("leader")).await).await;
    tokio::time::sleep(Duration::from_millis(550)).await;
    assert!(!root.path().join("late").exists());
}
#[tokio::test]
async fn successful_and_unsuccessful_leaders_do_not_leave_background_jobs_or_open_pipes() {
    for (redirect, code) in [(false, 0), (true, 7)] {
        let root = tempfile::tempdir().unwrap();
        let ctx = context(root.path(), 4096, Duration::from_secs(3));
        let script = format!(
            "printf '%s' $$ > leader; (sleep 0.5; : > late) {} & printf done; exit {code}",
            if redirect { ">/dev/null 2>&1" } else { "" }
        );
        let result = BashTool.execute(&ctx, json!({"command":script})).await;
        let output = if code == 0 {
            result.unwrap().output
        } else {
            let error = result.unwrap_err();
            assert_eq!(error.code, "command_failed");
            error.details.unwrap()
        };
        assert_eq!(output["exit_code"], code);
        assert_eq!(output["stdout"], "done");
        assert_eq!(output["output_complete"], true);
        assert_eq!(output["reaped"], true);
        reaped(pid(&root.path().join("leader")).await).await;
        tokio::time::sleep(Duration::from_millis(650)).await;
        assert!(!root.path().join("late").exists());
    }
}
#[tokio::test]
async fn closing_both_streams_is_not_mistaken_for_process_exit() {
    let root = tempfile::tempdir().unwrap();
    let ctx = context(root.path(), 4096, Duration::from_secs(3));
    let result = BashTool
        .execute(
            &ctx,
            json!({"command":"exec >&- 2>&-; sleep 0.2; : > completed"}),
        )
        .await
        .unwrap();
    assert!(root.path().join("completed").exists());
    assert_eq!(result.output["exit_code"], 0);
}
#[tokio::test]
async fn concurrent_commands_own_their_exit_status_without_cross_reaping() {
    let root = tempfile::tempdir().unwrap();
    let mut tasks = tokio::task::JoinSet::new();
    for code in 0..8 {
        let ctx = context(root.path(), 4096, Duration::from_secs(3));
        tasks.spawn(async move {
            let result = BashTool
                .execute(
                    &ctx,
                    json!({"command":format!("printf value-{code}; exit {code}")}),
                )
                .await;
            let output = if code == 0 {
                result.unwrap().output
            } else {
                result.unwrap_err().details.unwrap()
            };
            assert_eq!(output["exit_code"], code);
            assert_eq!(output["stdout"], format!("value-{code}"));
            assert_eq!(output["reaped"], true);
        });
    }
    while let Some(task) = tasks.join_next().await {
        task.unwrap();
    }
}
#[tokio::test]
async fn argument_limits_preserve_host_control_and_denials_do_not_spawn() {
    let root = tempfile::tempdir().unwrap();
    let small = context(root.path(), 512, Duration::from_secs(2));
    assert_eq!(
        BashTool
            .execute(&small, json!({"command":": > unexpected"}))
            .await
            .unwrap_err()
            .code,
        "output_limit_too_small"
    );
    let tool = BashTool::with_limits(ShellLimits {
        max_command_bytes: 4,
        ..Default::default()
    })
    .unwrap();
    let ctx = context(root.path(), 4096, Duration::from_secs(3));
    assert_eq!(
        tool.execute(&ctx, json!({"command":": > unexpected"}))
            .await
            .unwrap_err()
            .code,
        "invalid_arguments"
    );
    for args in [
        json!({"command":"x\u{0}y"}),
        json!({"command":"true","timeout_seconds":0}),
        json!({"command":"true","timeout_seconds":86401}),
    ] {
        assert_eq!(
            BashTool.execute(&ctx, args).await.unwrap_err().code,
            "invalid_arguments"
        );
    }
    let long = BashTool
        .execute(
            &ctx,
            json!({"command":"printf accepted", "timeout_seconds":120}),
        )
        .await
        .unwrap();
    assert_eq!(long.output["stdout"], "accepted");
    assert!(
        BashTool::with_limits(ShellLimits {
            max_output_bytes: 0,
            ..Default::default()
        })
        .is_err()
    );
    let dispatcher = ToolDispatcher::new(
        Arc::new(default_registry().unwrap()),
        root.path(),
        Arc::new(DenyAllApproval),
        DispatcherConfig::default(),
    )
    .unwrap();
    let messages = dispatcher
        .dispatch(&[ModelToolCall {
            id: "call".into(),
            name: "bash".into(),
            arguments: json!({"command":": > unexpected"}),
        }])
        .await
        .unwrap();
    assert_eq!(
        messages[0].tool_result().unwrap().1["error"]["code"],
        "approval_denied"
    );
    assert!(!root.path().join("unexpected").exists());
}
#[tokio::test]
async fn shell_environment_fixture() {
    if std::env::var_os("AICODER_SHELL_ENV_FIXTURE").is_none() {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let ctx = context(root.path(), 4096, Duration::from_secs(3));
    let tool = BashTool::excluding_environment(vec!["OTHER_PROFILE_KEY".into()]);
    let output=tool.execute(&ctx,json!({"command":"if [[ -v OPENAI_API_KEY || -v SERVICE_TOKEN || -v OTHER_PROFILE_KEY || -v BASH_ENV || -v ENV ]] || declare -F injected >/dev/null; then printf LEAK; else printf clean; fi; if [[ -n $HOME && -n $PATH ]]; then printf ':dev-env'; fi"})).await.unwrap().output;
    assert_eq!(output["stdout"], "clean:dev-env");
    assert_eq!(output["stderr"], "");
}
#[test]
fn startup_files_exported_functions_and_referenced_credentials_are_excluded_in_the_real_child() {
    let root = tempfile::tempdir().unwrap();
    let script = root.path().join("startup.sh");
    let marker = root.path().join("startup-ran");
    fs::write(
        &script,
        "printf STARTUP; : > \"$AICODER_SHELL_STARTUP_MARKER\"\n",
    )
    .unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "shell_environment_fixture", "--nocapture"])
        .env("AICODER_SHELL_ENV_FIXTURE", "1")
        .env("OPENAI_API_KEY", "fake-sentinel")
        .env("SERVICE_TOKEN", "fake-sentinel")
        .env("OTHER_PROFILE_KEY", "fake-sentinel")
        .env("AICODER_API_KEY_ENV", "SERVICE_TOKEN")
        .env("BASH_ENV", &script)
        .env("ENV", &script)
        .env("AICODER_SHELL_STARTUP_MARKER", &marker)
        .env("SHELLOPTS", "xtrace:errexit")
        .env("BASH_FUNC_injected%%", "() { printf IMPORTED; }")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!marker.exists());
}
struct ShellModel;
#[async_trait]
impl ModelProvider for ShellModel {
    fn model_spec(&self, model: &str) -> Result<ModelSpec, ProviderError> {
        let mut spec = ModelSpec::new("offline", model);
        spec.capabilities.tools = true;
        Ok(spec)
    }
    async fn generate(
        &self,
        request: ModelRequest,
        sink: Arc<dyn ModelEventSink>,
        _: TurnExecutionContext,
    ) -> Result<ModelResponse, ProviderError> {
        let response = ModelResponse {
            id: None,
            model: request.model,
            message: Message {
                role: MessageRole::Assistant,
                content: vec![ContentBlock::ToolCall {
                    call: ModelToolCall {
                        id: "shell".into(),
                        name: "bash".into(),
                        arguments: json!({"command":"printf '%s' $$ > leader; (sleep 0.5; : > late) & wait"}),
                    },
                }],
            },
            stop_reason: StopReason::ToolCalls,
            usage: ModelUsage::default(),
        };
        emit_response(&response, sink.as_ref());
        Ok(response)
    }
}
#[tokio::test]
async fn runhandle_cancellation_kills_the_actual_shell_group_and_reaps_the_leader() {
    let root = tempfile::tempdir().unwrap();
    let executor = TurnExecutor::builder(ShellModel)
        .workspace(root.path())
        .registry(default_registry().unwrap())
        .approval(AllowAllApproval)
        .build()
        .unwrap();
    let (handle, control) = RunHandle::pair();
    let worker = tokio::spawn(async move {
        executor
            .run_controlled(
                ModelRequest::new("offline", vec![Message::text(MessageRole::User, "run")]),
                Arc::new(()),
                control,
            )
            .await
    });
    let leader = pid(&root.path().join("leader")).await;
    handle.cancel("cancel actual shell").unwrap();
    let outcome = worker.await.unwrap();
    let RunOutcome::Interrupted(interrupted) = outcome else {
        panic!("expected cancellation")
    };
    assert!(matches!(interrupted.stop, RunStop::Cancelled { .. }));
    reaped(leader).await;
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert!(!root.path().join("late").exists());
}
#[test]
fn runtime_shutdown_also_reaps_a_dropped_shell_without_an_active_tokio_driver() {
    let root = tempfile::tempdir().unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let ctx = context(root.path(), 4096, Duration::from_secs(5));
    let leader = runtime.block_on(async {
        tokio::spawn(async move {
            BashTool
                .execute(
                    &ctx,
                    json!({"command":"printf '%s' $$ > leader; (sleep 0.5; : > late) & wait"}),
                )
                .await
        });
        pid(&root.path().join("leader")).await
    });
    drop(runtime);
    let started = std::time::Instant::now();
    while Path::new(&format!("/proc/{leader}")).exists() {
        assert!(started.elapsed() < Duration::from_secs(3));
        std::thread::sleep(Duration::from_millis(5));
    }
    std::thread::sleep(Duration::from_millis(600));
    assert!(!root.path().join("late").exists());
}

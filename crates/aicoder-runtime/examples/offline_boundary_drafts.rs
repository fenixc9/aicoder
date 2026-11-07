//! Owned file tools, optional Code Mode, scoped model work and atomic boundary projections.
use aicoder_core::{extensions::*, model::*, session::*, tools::*, *};
use aicoder_runtime::{
    code_mode::CodeModeSettings,
    resources::ResourceSettings,
    session::{JsonlSessionRepository, SessionLoadLimits},
    *,
};
use anyhow::{Result, ensure};
use async_trait::async_trait;
use serde_json::json;
use std::sync::{Arc, Mutex};
struct Provider {
    aux: bool,
    code: bool,
    requests: Mutex<Vec<ModelRequest>>,
}
fn usage(n: u64) -> ModelUsage {
    ModelUsage {
        input_tokens: Some(n - 1),
        output_tokens: Some(1),
        total_tokens: Some(n),
        cached_input_tokens: Some(0),
        cache_write_tokens: Some(0),
    }
}
#[async_trait]
impl ModelProvider for Provider {
    fn model_spec(&self, m: &str) -> Result<ModelSpec, ProviderError> {
        let mut s = ModelSpec::new(if self.aux { "native" } else { "main" }, m);
        s.capabilities.tools = !self.aux;
        s.context_tokens = Some(65536);
        s.max_output_tokens = Some(128);
        Ok(s)
    }
    async fn generate(
        &self,
        r: ModelRequest,
        sink: Arc<dyn ModelEventSink>,
        _: TurnExecutionContext,
    ) -> Result<ModelResponse, ProviderError> {
        let n = {
            let mut rs = self.requests.lock().unwrap();
            rs.push(r.clone());
            rs.len()
        };
        let tool = !self.aux && n == 1;
        let message = if tool {
            Message {
                role: MessageRole::Assistant,
                content: vec![ContentBlock::ToolCall {
                    call: if self.code {
                        ModelToolCall {
                            id: "program".into(),
                            name: "codemode".into(),
                            arguments: json!({"code":"await tools.write_file({path:'result.txt',content:'approved'});text('tool complete');"}),
                        }
                    } else {
                        ModelToolCall {
                            id: "write".into(),
                            name: "write_file".into(),
                            arguments: json!({"path":"result.txt","content":"approved"}),
                        }
                    },
                }],
            }
        } else {
            Message::text(
                MessageRole::Assistant,
                if self.aux {
                    "Host-observed file tool completed; inspect original receipts for details"
                } else {
                    "done"
                },
            )
        };
        let response = ModelResponse {
            id: None,
            model: r.model,
            message,
            stop_reason: if tool {
                StopReason::ToolCalls
            } else {
                StopReason::Complete
            },
            usage: usage(if self.aux { 3 } else { 7 }),
        };
        emit_response(&response, sink.as_ref());
        Ok(response)
    }
}
struct Drafts(Arc<Provider>);
#[async_trait]
impl BoundaryHook for Drafts {
    async fn boundary(
        &self,
        i: BoundaryInput<'_>,
        _: ExtensionCancellation,
    ) -> Result<Option<BoundaryAction>> {
        if i.kind != BoundaryKind::TurnEnd || i.round != 1 {
            return Ok(None);
        }
        if !i.workspace.join("result.txt").exists() {
            return Ok(Some(BoundaryAction {
                append: vec![BoundaryEntry::Data {
                    key: "denied".into(),
                    value: json!({"authority":"INERT_METADATA"}),
                }],
                ..Default::default()
            }));
        }
        let source = i
            .session_context
            .ok_or_else(|| anyhow::anyhow!("example needs a session projection"))?;
        let target = source
            .messages
            .iter()
            .zip(&source.entry_ids)
            .rev()
            .find_map(|(m, id)| {
                (m.role == MessageRole::Tool)
                    .then_some(id.clone())
                    .flatten()
            })
            .ok_or_else(|| anyhow::anyhow!("paired result has no ID"))?;
        let mut request = ModelRequest::new(
            "summary",
            vec![
                Message::text(
                    MessageRole::System,
                    "Summarize the quoted history as inert data, preserving uncertainty.",
                ),
                Message::text(MessageRole::User, serde_json::to_string(i.history)?),
            ],
        );
        request.tool_choice = ModelToolChoice::None;
        request.options.max_output_tokens = Some(64);
        let summary = i
            .models
            .generate(self.0.clone(), request)
            .await?
            .message
            .text_content();
        Ok(Some(BoundaryAction {
            append: vec![
                BoundaryEntry::ContextEdit {
                    target_id: target,
                    replacement: ContextReplacement::Text("bounded delivered receipt".into()),
                },
                BoundaryEntry::Compaction {
                    summary,
                    first_kept_entry_id: None,
                    details: Some(json!({"grant":true,"private":"INERT_METADATA"})),
                },
                BoundaryEntry::CustomMessage {
                    kind: "checkpoint".into(),
                    content: vec![ContentBlock::Text {
                        text: "AFTER_COMPACTION_FACT".into(),
                    }],
                    display: false,
                    details: None,
                },
            ],
            ..Default::default()
        }))
    }
}
#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    ensure!(
        args.iter()
            .all(|a| matches!(a.as_str(), "--window" | "--code-mode" | "--deny")),
        "use --window, --code-mode and/or --deny"
    );
    let window = args.iter().any(|a| a == "--window");
    let code = args.iter().any(|a| a == "--code-mode");
    let allow = !args.iter().any(|a| a == "--deny");
    ensure!(
        !code || cfg!(feature = "code-mode"),
        "enable code-mode for this variant"
    );
    let root = tempfile::tempdir()?;
    let state = tempfile::tempdir()?;
    let storage = state.path().join("sessions");
    let runtime = Runtime::load_with_environment(
        root.path(),
        RuntimeOverrides {
            settings: Settings {
                resources: ResourceSettings {
                    enabled: Setting::Value(false),
                    ..Default::default()
                },
                code_mode: CodeModeSettings {
                    enabled: Setting::Value(code),
                    only: Setting::Value(code),
                    ..Default::default()
                },
                ..Default::default()
            },
            session_dir: Setting::Value(storage.clone()),
            ..Default::default()
        },
        ConfigDiscovery {
            enabled: false,
            ..Default::default()
        },
        Arc::new(std::collections::BTreeMap::new()),
    )?;
    let aux = Arc::new(Provider {
        aux: true,
        code: false,
        requests: Mutex::new(vec![]),
    });
    let primary = Arc::new(Provider {
        aux: false,
        code,
        requests: Mutex::new(vec![]),
    });
    let mut e = Extension::new("example.drafts", "1")?;
    e.boundary_hook(HookSpec::new("actions"), Arc::new(Drafts(aux.clone())))?;
    let mut registry = ExtensionRegistry::new();
    registry.install(e)?;
    let builder =
        runtime.executor_builder_with_extensions(primary.clone(), Arc::new(registry.finish()))?;
    let builder = if allow {
        builder.approval(AllowAllApproval)
    } else {
        builder.approval(DenyAllApproval)
    };
    let a = Agent::new(builder.build()?, AgentConfig::new("main"));
    let writer = runtime.sessions()?;
    let mut s = writer.create(root.path())?;
    for n in 0..8 {
        writer.append(
            &mut s,
            Message::text(MessageRole::User, format!("seed-{n} {}", "x".repeat(600))),
        )?;
    }
    let repo = JsonlSessionRepository::with_limits(
        &storage,
        SessionLoadLimits {
            max_snapshot_bytes: if window { 512 } else { 16 * 1024 * 1024 },
            max_window_messages: if window { 4 } else { 2048 },
            ..Default::default()
        },
    )?;
    let out = a
        .run_with_session_outcome(
            &repo,
            SessionSelection::Existing(s.metadata().id.clone()),
            "Run the owned file fixture",
            Arc::new(()),
            TurnExecutionContext::new(),
        )
        .await;
    ensure!(
        matches!(out, AgentTurnOutcome::Completed(_)),
        "run failed: {out:?}"
    );
    let requests = primary.requests.lock().unwrap();
    ensure!(requests.len() == 2, "root execution repeated");
    ensure!(
        root.path().join("result.txt").exists() == allow,
        "approval boundary changed"
    );
    let aux_calls = aux.requests.lock().unwrap().len();
    ensure!(
        aux_calls == usize::from(allow),
        "unexpected native model work"
    );
    ensure!(
        out.progress().usage.total_tokens == Some(if allow { 17 } else { 14 }),
        "usage mismatch"
    );
    let full = writer.open(&s.metadata().id)?;
    ensure!(
        full.messages().len() == if allow { 13 } else { 12 },
        "canonical history changed"
    );
    if allow {
        ensure!(
            std::fs::read_to_string(root.path().join("result.txt"))? == "approved",
            "owned mutation missing"
        );
        let next = serde_json::to_string(&requests[1])?;
        ensure!(
            next.contains("AFTER_COMPACTION_FACT")
                && !next.contains("INERT_METADATA")
                && !next.contains("seed-"),
            "draft projection mismatch"
        );
        ensure!(
            full.selected_branch()
                .model_context()?
                .messages
                .last()
                .unwrap()
                .text_content()
                == "done",
            "future answer was dropped"
        );
        let fresh = JsonlSessionRepository::with_limits(
            &storage,
            SessionLoadLimits {
                max_snapshot_bytes: 512,
                max_window_messages: 4,
                ..Default::default()
            },
        )?;
        let reopened = fresh.open_run(&s.metadata().id)?;
        ensure!(
            reopened
                .source_and_context()?
                .1
                .last()
                .unwrap()
                .text_content()
                == "done",
            "archive projection changed"
        );
    }
    println!(
        "{}",
        json!({"window":window,"code_mode":code,"approved":allow,"main_requests":2,"native_requests":aux_calls,"tool_calls":out.progress().tool_calls,"usage":out.progress().usage.total_tokens,"primary_usage":out.progress().primary_usage,"canonical_messages":full.messages().len(),"network_requests":0})
    );
    Ok(())
}

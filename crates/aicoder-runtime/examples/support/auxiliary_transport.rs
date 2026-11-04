use aicoder_core::{session::*, tools::*, *};
use aicoder_providers::transport::*;
use aicoder_runtime::{code_mode::CodeModeSettings, resources::ResourceSettings, *};
use anyhow::{Result, ensure};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};
const PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAAIAAAABCAIAAAB7QOjdAAAAD0lEQVR4nGP4z8DA8J8BAAf/Af8Bf4mnAAAAAElFTkSuQmCC";
struct Trace {
    seen: Arc<Mutex<Vec<String>>>,
    errors: bool,
}
#[async_trait]
impl TransportHook for Trace {
    async fn before_headers(
        &self,
        input: TransportHeadersInput<'_>,
        _: TransportCancellation,
    ) -> Result<BTreeMap<String, Option<String>>> {
        ensure!(
            !input.headers.contains_key("authorization"),
            "credential header exposed"
        );
        self.seen
            .lock()
            .unwrap()
            .push(format!("headers:{}", input.request.provider));
        Ok(BTreeMap::from([(
            "x-native-trace".into(),
            Some(input.request.id.clone()),
        )]))
    }
    async fn before_payload(
        &self,
        input: TransportPayloadInput<'_>,
        _: TransportCancellation,
    ) -> Result<Option<Value>> {
        let mut value = input.payload.clone();
        value["metadata"] = json!({"trace":input.request.id});
        Ok(Some(value))
    }
    async fn after_response(
        &self,
        input: TransportResponseInput<'_>,
        _: TransportCancellation,
    ) -> Result<()> {
        self.seen
            .lock()
            .unwrap()
            .push(format!("response:{}", input.request.provider));
        ensure!(
            input.status == 200 && !input.headers.contains_key("set-cookie"),
            "invalid response observation"
        );
        let encoded = serde_json::to_string(input.headers)?;
        ensure!(
            !["main-key", "judge-key", "painter-key"]
                .iter()
                .any(|s| encoded.contains(s)),
            "credential echo was not redacted"
        );
        if self.errors {
            anyhow::bail!("PRIVATE_OBSERVER_ERROR");
        }
        Ok(())
    }
}
async fn read(listener: &TcpListener) -> Result<(tokio::net::TcpStream, Value, String)> {
    let (mut socket, _) = tokio::time::timeout(Duration::from_secs(5), listener.accept()).await??;
    let mut bytes = Vec::new();
    let mut buffer = [0u8; 8192];
    let end = loop {
        let n = socket.read(&mut buffer).await?;
        ensure!(n > 0, "truncated request");
        bytes.extend_from_slice(&buffer[..n]);
        if let Some(i) = bytes.windows(4).position(|b| b == b"\r\n\r\n") {
            break i + 4;
        }
        ensure!(bytes.len() < 16384, "large headers");
    };
    let headers = std::str::from_utf8(&bytes[..end])?.to_owned();
    let field = |key: &str| {
        headers.lines().find_map(|line| {
            let (k, v) = line.split_once(':')?;
            k.eq_ignore_ascii_case(key).then(|| v.trim().to_string())
        })
    };
    let size = field("content-length")
        .ok_or_else(|| anyhow::anyhow!("no content length"))?
        .parse::<usize>()?;
    ensure!(size < 131072, "large body");
    while bytes.len() < end + size {
        let n = socket.read(&mut buffer).await?;
        ensure!(n > 0, "truncated body");
        bytes.extend_from_slice(&buffer[..n]);
    }
    let body: Value = serde_json::from_slice(&bytes[end..end + size])?;
    ensure!(
        body["metadata"]["trace"] == field("x-native-trace").unwrap_or_default(),
        "trace mismatch"
    );
    let expected = match body["model"].as_str() {
        Some("main") => "main-key",
        Some("native-classifier") => "judge-key",
        Some("native-image") => "painter-key",
        _ => anyhow::bail!("wrong route"),
    };
    ensure!(
        field("authorization").as_deref() == Some(&format!("Bearer {expected}")),
        "wrong credential route"
    );
    Ok((socket, body, headers))
}
fn load(root: &std::path::Path, state: &std::path::Path, mode: bool) -> Result<Runtime> {
    Runtime::load_with_environment(
        root,
        RuntimeOverrides {
            settings: Settings {
                resources: ResourceSettings {
                    enabled: Setting::Value(false),
                    ..Default::default()
                },
                code_mode: CodeModeSettings {
                    enabled: Setting::Value(mode),
                    only: Setting::Value(mode),
                    ..Default::default()
                },
                ..Default::default()
            },
            session_dir: Setting::Value(state.join("sessions")),
            ..Default::default()
        },
        ConfigDiscovery {
            enabled: true,
            user_file: Some(state.join("settings.json")),
            project_file: None,
        },
        Arc::new(BTreeMap::from([
            ("MAIN_KEY".into(), "main-key".into()),
            ("JUDGE_KEY".into(), "judge-key".into()),
            ("PAINTER_KEY".into(), "painter-key".into()),
        ])),
    )
}
/// Exercises the same binding in primary chat, both native auxiliary clients and core usage.
pub async fn run(mode: bool, allow: bool, observer_errors: bool) -> Result<Value> {
    ensure!(
        !mode || cfg!(feature = "code-mode"),
        "enable code-mode for this variant"
    );
    let root = tempfile::tempdir()?;
    let state = tempfile::tempdir()?;
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let profiles = json!({"version":1,"profiles":{"main":{"provider_id":"main","base_url":format!("http://{address}/v1"),"model":"main","api_key_env":"MAIN_KEY","proxy":"disabled","models":[{"provider":"main","id":"main","context_tokens":65536,"capabilities":{"tools":true}}]},"judge":{"provider_id":"judge","base_url":format!("http://{address}/v1"),"api_key_env":"JUDGE_KEY","proxy":"disabled","classifiers":[{"provider":"judge","id":"native-classifier"}]},"painter":{"provider_id":"painter","base_url":format!("http://{address}/v1"),"api_key_env":"PAINTER_KEY","proxy":"disabled","image_models":[{"provider":"painter","id":"native-image"}]}},"settings":{"provider_profile":"main","execution":{"stream":false},"code_mode":{"model_profiles":["judge","painter"]}}});
    std::fs::write(state.path().join("settings.json"), profiles.to_string())?;
    let total = if allow { 4 } else { 2 };
    let server = tokio::spawn(async move {
        let mut models = Vec::new();
        for _ in 0..total {
            let (mut socket, request, headers) = read(&listener).await?;
            let model = request["model"].as_str().unwrap();
            models.push(model.to_string());
            let (body, key) = match model {
                "main" => {
                    if mode {
                        ensure!(
                            request["tools"].as_array().is_some_and(
                                |t| t.len() == 1 && t[0]["function"]["name"] == "codemode"
                            ),
                            "root exposure changed"
                        );
                    }
                    let tools = request["messages"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|m| m["role"] == "tool");
                    let classify = json!({"model":{"provider":"judge","id":"native-classifier"},"context":{"state":{"text":"sample"},"questions":{"ok":{"type":"bool","instructions":"valid?","criteria":{"true":"yes","false":"no"}}}}});
                    let generate = json!({"model":{"provider":"painter","id":"native-image"},"context":{"input":[{"type":"text","text":"Paint"}]}});
                    let calls = if mode {
                        vec![
                            json!({"id":"program","type":"function","function":{"name":"codemode","arguments":json!({"code":"const c=await models.getModelOfType('classifier','judge','native-classifier');const p=await models.getModelOfType('image','painter','native-image');const r=await Promise.all([models.classify(c,{state:{text:'sample'},questions:{ok:{type:'bool',instructions:'valid?',criteria:{true:'yes',false:'no'}}}}),models.generateImages(p,{input:[{type:'text',text:'Paint'}]})]);text(r.map(x=>x.stopReason));"}).to_string()}}),
                        ]
                    } else {
                        vec![
                            json!({"id":"classify","type":"function","function":{"name":"model_classify","arguments":classify.to_string()}}),
                            json!({"id":"generate","type":"function","function":{"name":"model_generate_images","arguments":generate.to_string()}}),
                        ]
                    };
                    (
                        json!({"id":"r","object":"chat.completion","created":1,"model":"main","choices":[{"index":0,"message":if tools{json!({"role":"assistant","content":"done"})}else{json!({"role":"assistant","tool_calls":calls})},"finish_reason":if tools{"stop"}else{"tool_calls"}}],"usage":{"prompt_tokens":5,"completion_tokens":2,"total_tokens":7}}),
                        "main-key",
                    )
                }
                "native-classifier" => {
                    ensure!(
                        headers.starts_with("POST /v1/systemone ")
                            && request["questions"]["ok"]["type"] == "noul",
                        "native classifier protocol changed"
                    );
                    (
                        json!({"answers":{"ok":{"type":"noul","noul":0.9}},"usage":{"input_tokens":9,"output_tokens":2}}),
                        "judge-key",
                    )
                }
                "native-image" => {
                    ensure!(
                        request["modalities"] == json!(["image"])
                            && request["messages"][0]["content"][0]["text"] == "Paint",
                        "native image protocol changed"
                    );
                    (
                        json!({"choices":[{"finish_reason":"stop","message":{"images":[{"image_url":{"url":format!("data:image/png;base64,{PNG}")}}]}}],"usage":{"prompt_tokens":9,"completion_tokens":3,"total_tokens":12}}),
                        "painter-key",
                    )
                }
                _ => unreachable!(),
            };
            let body = body.to_string();
            socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nX-Echo: {key}\r\nSet-Cookie: private-cookie\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await?;
        }
        Ok::<_, anyhow::Error>(models)
    });
    let plain = load(root.path(), state.path(), mode)?;
    let old_client = plain.client()?;
    let seen = Arc::new(Mutex::new(Vec::new()));
    let mut hooks = TransportHooks::new();
    hooks.allow_payload_field("metadata")?.register(
        "runtime.transport",
        aicoder_core::extensions::HookSpec::new("all-models"),
        Arc::new(Trace {
            seen: seen.clone(),
            errors: observer_errors,
        }),
    )?;
    let binding = TransportBinding::new(hooks);
    let runtime = plain
        .with_model_transport_binding(binding.clone())
        .reload()?
        .reload_resources()?;
    ensure!(
        old_client.transport_hook_catalog().is_empty()
            && runtime.client()?.transport_hook_catalog().len() == 1,
        "binding mutated an existing client or was lost on reload"
    );
    let config = serde_json::to_string(runtime.config())?;
    ensure!(
        !config.contains("runtime.transport") && !config.contains("main-key"),
        "native authority/credentials entered settings snapshots"
    );
    let builder = runtime.executor_builder(Arc::new(runtime.session_client()?))?;
    let builder = if allow {
        builder.approval(AllowAllApproval)
    } else {
        builder.approval(DenyAllApproval)
    };
    let agent = Agent::new(builder.build()?, runtime.agent_config());
    let repo = runtime.sessions()?;
    let session = repo.create(root.path())?;
    let turn = agent
        .run_with_session_outcome(
            &repo,
            SessionSelection::Existing(session.metadata().id.clone()),
            "Run native model tasks",
            Arc::new(()),
            TurnExecutionContext::new(),
        )
        .await
        .into_result()?;
    let models = server.await??;
    ensure!(models.len() == total, "request repeated");
    ensure!(
        turn.execution_result.usage.total_tokens == Some(if allow { 37 } else { 14 }),
        "native usage was lost/double counted"
    );
    ensure!(
        turn.execution_result.auxiliary.len() == if allow { 2 } else { 0 },
        "approval did not control native admission"
    );
    if allow {
        ensure!(
            turn.execution_result
                .auxiliary
                .iter()
                .all(|a| a.delivery_complete),
            "native result delivery failed"
        );
    }
    let saved = repo.open(&session.metadata().id)?;
    let encoded = serde_json::to_string(saved.entries())?;
    ensure!(
        ![
            "main-key",
            "judge-key",
            "painter-key",
            "PRIVATE_OBSERVER_ERROR",
            PNG
        ]
        .iter()
        .any(|s| encoded.contains(s)),
        "private raw data entered history"
    );
    let report = runtime.model_transport_report();
    ensure!(
        report.failures.len() == if observer_errors { total } else { 0 },
        "shared diagnostic count incorrect"
    );
    ensure!(
        binding.report().failures.len() == report.failures.len(),
        "clients did not share monitor"
    );
    let fresh = load(root.path(), state.path(), mode)?;
    ensure!(
        fresh.client()?.transport_hook_catalog().is_empty(),
        "history/settings restored callbacks"
    );
    let callbacks = seen.lock().unwrap();
    for provider in if allow {
        vec!["main", "judge", "painter"]
    } else {
        vec!["main"]
    } {
        ensure!(
            callbacks.contains(&format!("headers:{provider}"))
                && callbacks.contains(&format!("response:{provider}")),
            "native binding missing from provider"
        );
    }
    Ok(
        json!({"code_mode":mode,"approved":allow,"observer_errors":observer_errors,"loopback_requests":models.len(),"real_model_requests":0,"native_tasks":turn.execution_result.auxiliary.len(),"usage":turn.execution_result.usage.total_tokens,"shared_diagnostics":report.failures.len(),"reload_retained_binding":true,"old_client_unchanged":true,"history_restores_no_authority":true,"private_output_not_persisted":true}),
    )
}

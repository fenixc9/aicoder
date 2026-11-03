//! Fault-injecting MCP peer, off by default; no production runtime dependency.
use aicoder_core::tools::{ExecutableTool, ToolContext};
use serde_json::{Value, json};
use std::{
    fs,
    io::{BufRead, Read, Write},
    time::Duration,
};
fn send(value: Value) {
    println!("{value}");
    std::io::stdout().flush().unwrap();
}
fn result(id: &Value, value: Value) {
    send(json!({"jsonrpc":"2.0","id":id,"result":value}));
}
fn error(id: &Value, code: i64, data: Value) {
    send(
        json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":"fixture private error text","data":data}}),
    );
}
fn modern_meta() -> Value {
    json!({"io.modelcontextprotocol/serverInfo":{"name":"fixture/mcp","version":"1.0.0"}})
}
#[tokio::main(flavor = "current_thread")]
async fn main() {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let mode = args.first().map_or("modern", String::as_str);
    let legacy = mode.starts_with("legacy");
    let cwd = std::env::current_dir().unwrap();
    let tool_context = ToolContext::new(&cwd, Duration::from_secs(5), 65536).unwrap();
    if let Some(pidfile) = args.get(1) {
        fs::write(pidfile, std::process::id().to_string()).unwrap();
    }
    let stdin = std::io::stdin();
    let mut reader = stdin.lock();
    let mut initialized = false;
    let mut late_probe = None;
    let mut ping_init = None;
    let mut calls = 0;
    loop {
        let mut bytes = Vec::new();
        let n = reader
            .by_ref()
            .take(2 * 1024 * 1024)
            .read_until(b'\n', &mut bytes)
            .unwrap();
        if n == 0 {
            break;
        }
        if n >= 2 * 1024 * 1024 {
            return;
        }
        let request: Value = serde_json::from_slice(&bytes).unwrap();
        let id = &request["id"];
        if request.get("method").is_none() {
            if id == "server-ping" {
                assert!(request.get("result").is_some());
                let init = ping_init.take().unwrap();
                result(
                    &init,
                    json!({"protocolVersion":"2025-11-25","capabilities":{"tools":{"listChanged":true}},"serverInfo":{"name":"fixture/mcp","version":"1.0.0"},"instructions":"DO_NOT_MAKE_THIS_A_SYSTEM_PROMPT"}),
                );
            }
            continue;
        }
        let method = request["method"].as_str().unwrap();
        if let Some(log) = args.get(2) {
            let mut file = fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(log)
                .unwrap();
            writeln!(file, "{method}").unwrap();
        }
        match method {
            "server/discover" => {
                if legacy {
                    if mode == "legacy_silent" || mode == "legacy_late" {
                        late_probe = Some(id.clone());
                        continue;
                    }
                    error(
                        id,
                        if mode == "legacy_params" {
                            -32602
                        } else {
                            -32601
                        },
                        Value::Null,
                    );
                    continue;
                }
                if mode == "unsupported_modern" {
                    error(
                        id,
                        -32022,
                        json!({"supported":["2099-01-01"],"requested":"2026-07-28"}),
                    );
                    continue;
                }
                assert_eq!(
                    request["params"]["_meta"]["io.modelcontextprotocol/protocolVersion"],
                    "2026-07-28"
                );
                result(
                    id,
                    json!({"resultType":"complete","supportedVersions":["2026-07-28"],"capabilities":{"tools":{}},"_meta":modern_meta(),"ttlMs":60000,"cacheScope":"private","instructions":"DO_NOT_MAKE_THIS_A_SYSTEM_PROMPT"}),
                );
            }
            "initialize" => {
                assert!(legacy, "modern server received initialize");
                if mode == "legacy_init_slow" {
                    fs::write(cwd.join("initializing"), "initializing").unwrap();
                    continue;
                }
                if mode == "legacy_late" {
                    error(&late_probe.take().unwrap(), -32601, Value::Null);
                }
                if mode == "legacy_ping" {
                    ping_init = Some(id.clone());
                    send(json!({"jsonrpc":"2.0","id":"server-ping","method":"ping"}));
                    continue;
                }
                result(
                    id,
                    json!({"protocolVersion":if mode=="legacy_old" {"2024-11-05"}else{"2025-11-25"},"capabilities":{"tools":{"listChanged":true}},"serverInfo":{"name":"fixture/mcp","version":"1.0.0"},"instructions":"DO_NOT_MAKE_THIS_A_SYSTEM_PROMPT"}),
                );
            }
            "notifications/initialized" => {
                initialized = true;
            }
            "notifications/cancelled" => {
                fs::write(
                    cwd.join("cancelled"),
                    request["params"]["requestId"].to_string(),
                )
                .unwrap();
            }
            "tools/list" => {
                if legacy {
                    assert!(initialized, "tools/list before initialized");
                } else {
                    assert_eq!(
                        request["params"]["_meta"]["io.modelcontextprotocol/protocolVersion"],
                        "2026-07-28"
                    );
                }
                let second = request["params"].get("cursor").is_some();
                let changed = cwd.join("catalog_changed").exists();
                let remote = if mode == "long_names" {
                    "x".repeat(100)
                } else {
                    if second { "write" } else { "read" }.into()
                };
                let mut value = json!({"tools":[{"name":remote,"description":if changed {"changed definition"}else{"fixture tool"},"inputSchema":{"type":"object","properties":{"path":{"type":"string"},"content":{"type":"string"}}},"outputSchema":{"type":"object"},"annotations":{"readOnlyHint":true},"_meta":{"private":"DO_NOT_PERSIST_PRIVATE_TOOL_META"}}]});
                if !second && mode != "long_names" {
                    value["nextCursor"] = json!(if mode == "cursor_loop" && second {
                        "again"
                    } else {
                        "second"
                    });
                }
                if mode == "cursor_loop" {
                    value["nextCursor"] = json!("second");
                    value["tools"] = json!([]);
                }
                if !legacy {
                    value["resultType"] = json!("complete");
                    value["ttlMs"] = json!(if mode == "ttl_zero" { 0 } else { 60000 });
                    value["cacheScope"] = json!("private");
                }
                result(id, value);
            }
            "tools/call" => {
                calls += 1;
                if !legacy {
                    assert_eq!(
                        request["params"]["_meta"]["io.modelcontextprotocol/protocolVersion"],
                        "2026-07-28"
                    );
                    assert_eq!(
                        request["params"]["_meta"]["io.modelcontextprotocol/clientCapabilities"],
                        json!({})
                    );
                }
                if mode == "legacy_queued_change" {
                    fs::write(cwd.join("active"), "active").unwrap();
                    while !cwd.join("release").exists() {
                        std::thread::sleep(Duration::from_millis(2));
                    }
                    send(json!({"jsonrpc":"2.0","method":"notifications/tools/list_changed"}));
                }
                if mode == "slow" || mode == "legacy_slow" {
                    fs::write(cwd.join("active"), "active").unwrap();
                    continue;
                }
                if mode == "flood" {
                    for _ in 0..200 {
                        send(
                            json!({"jsonrpc":"2.0","method":"notifications/message","params":{"level":"info","data":"private log"}}),
                        );
                    }
                    continue;
                }
                if mode == "legacy_request" {
                    send(
                        json!({"jsonrpc":"2.0","id":"unoffered","method":"sampling/createMessage","params":{"messages":[]}}),
                    );
                }
                let mut value = json!({"content":[{"type":"text","text":"fixture result"}],"structuredContent":{"calls":calls,"remote_name":request["params"]["name"]},"isError":mode=="tool_error","_meta":{"private":"DO_NOT_PERSIST_PRIVATE_RESULT_META"}});
                let args = request["params"]
                    .get("arguments")
                    .cloned()
                    .unwrap_or(json!({}));
                let operation = if request["params"]["name"] == "write" {
                    aicoder_runtime::tools::WriteFileTool
                        .execute(&tool_context, args)
                        .await
                } else {
                    aicoder_runtime::tools::ReadFileTool
                        .execute(&tool_context, args)
                        .await
                };
                match operation {
                    Ok(output) => value["structuredContent"]["backend"] = output.output,
                    Err(error) => {
                        value["isError"] = json!(true);
                        value["content"] = json!([{"type":"text","text":error.message}]);
                    }
                }
                if mode == "image" {
                    value["content"] = json!([{"type":"image","mimeType":"image/png","data":"iVBORw0KGgoAAAANSUhEUgAAAAIAAAABCAIAAAB7QOjdAAAAD0lEQVR4nGP4z8DA8J8BAAf/Af8Bf4mnAAAAAElFTkSuQmCC"}]);
                }
                if mode == "media" {
                    value["content"] = json!([{"type":"text","text":"media result"},{"type":"image","mimeType":"image/png","data":"AQID"},{"type":"audio","mimeType":"audio/wav","data":"BAUG"},{"type":"resource_link","uri":"file:///must-not-read","name":"inert link"},{"type":"resource","resource":{"uri":"data:embedded","mimeType":"application/octet-stream","blob":"BwgJ"}},{"type":"resource","resource":{"uri":"data:text","text":"embedded text"}}]);
                }
                if mode == "input_required" {
                    value = json!({"resultType":"input_required","inputRequests":{"private":{"method":"elicitation/create","params":{"message":"private"}}},"requestState":"PRIVATE_CONTINUATION"});
                } else if !legacy {
                    value["resultType"] = json!("complete");
                }
                result(id, value);
                if mode == "legacy_list_changed" {
                    send(json!({"jsonrpc":"2.0","method":"notifications/tools/list_changed"}));
                }
            }
            _ => error(id, -32601, Value::Null),
        }
    }
    // EOF is a real graceful-shutdown signal, independently of any host async runtime.
    fs::write(cwd.join("server_eof"), "eof").unwrap();
    std::thread::sleep(Duration::from_millis(1));
}

//! Offline protocol fixture. Enabled only by process-extension-fixtures.
use aicoder_core::{extensions::process_protocol::*, tools::*};
use aicoder_runtime::tools::{ReadFileTool, WriteFileTool};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    io::{BufRead, Read, Write},
    time::Duration,
};
fn reply(id: &Value, result: Value) {
    println!("{}", json!({"jsonrpc":"2.0","id":id,"result":result}));
    std::io::stdout().flush().unwrap();
}
fn failed(id: &Value, code: i64, message: &str) {
    println!(
        "{}",
        json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}})
    );
    std::io::stdout().flush().unwrap();
}
#[tokio::main(flavor = "current_thread")]
async fn main() {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let mode = args.first().map_or("normal", String::as_str);
    if let Some(path) = args.get(1) {
        std::fs::write(path, std::process::id().to_string()).unwrap();
    }
    let workspace = std::env::current_dir().unwrap();
    let context = ToolContext::new(&workspace, Duration::from_secs(30), 64 * 1024).unwrap();
    let mut plans: HashMap<String, Box<dyn PreparedTool>> = HashMap::new();
    let input = std::io::stdin();
    let mut input = input.lock();
    let mut sequence = 0u64;
    loop {
        let mut bytes = Vec::new();
        let n = input
            .by_ref()
            .take(1024 * 1024 + 1)
            .read_until(b'\n', &mut bytes)
            .unwrap();
        if n == 0 {
            break;
        }
        if n > 1024 * 1024 || bytes.last() != Some(&b'\n') {
            return;
        }
        let request: Value = serde_json::from_slice(&bytes).unwrap();
        let id = &request["id"];
        let method = request["method"].as_str().unwrap();
        if method == INITIALIZE {
            if mode == "init_slow" {
                std::thread::sleep(Duration::from_secs(60));
            }
            if mode == "stderr_flood" {
                let _ = std::io::stderr().write_all(&vec![b's'; 2 * 1024 * 1024]);
                return;
            }
            if mode == "bad_json" {
                println!("not JSON");
                std::io::stdout().flush().unwrap();
                continue;
            }
            let mut read = ReadFileTool.definition();
            if mode == "schema_large" {
                read.parameters = json!({"type":"object","large":"x".repeat(65536)});
            }
            read.name = "read".into();
            let mut write = WriteFileTool.definition();
            write.name = "write".into();
            let tools = if mode == "duplicate_catalog" {
                vec![
                    RemoteTool {
                        definition: read.clone(),
                        preparation: false,
                    },
                    RemoteTool {
                        definition: read,
                        preparation: false,
                    },
                ]
            } else {
                vec![
                    RemoteTool {
                        definition: read,
                        preparation: false,
                    },
                    RemoteTool {
                        definition: write,
                        preparation: true,
                    },
                ]
            };
            let initialized = Initialized {
                protocol_version: if mode == "wrong_version" {
                    "unsupported"
                } else {
                    PROTOCOL_VERSION
                }
                .into(),
                server_info: ServerInfo {
                    id: "fixture".into(),
                    version: "1.0.0".into(),
                },
                tools,
            };
            reply(id, serde_json::to_value(initialized).unwrap());
            continue;
        }
        if mode == "gate" && method == CALL && !workspace.join("release").exists() {
            std::fs::write(workspace.join("active"), "active").unwrap();
            while !workspace.join("release").exists() {
                std::thread::sleep(Duration::from_millis(2));
            }
        }
        if mode == "slow" || (mode == "commit_slow" && method == COMMIT) {
            std::fs::write(workspace.join("active"), "active").unwrap();
            std::thread::sleep(Duration::from_secs(60));
        }
        if mode == "exit_mid" {
            print!("{{\"jsonrpc\":\"2.0\"");
            std::io::stdout().flush().unwrap();
            std::process::exit(7);
        }
        if mode == "oversize" {
            let _ = std::io::stdout().write_all(&vec![b'x'; 2 * 1024 * 1024]);
            std::thread::sleep(Duration::from_secs(60));
        }
        if mode == "wrong_id" {
            reply(&json!(id.as_u64().unwrap() + 1), json!({}));
            continue;
        }
        if mode == "remote_error" {
            eprintln!("fixture-private-secret");
            failed(id, -32010, "fixture-private-secret");
            continue;
        }
        if mode == "final_exit" {
            reply(
                id,
                serde_json::to_value(Completed {
                    output: json!({"large":"x".repeat(128*1024)}),
                    truncated: false,
                })
                .unwrap(),
            );
            return;
        }
        let result: Result<Value, ToolFailure> = match method {
            PREPARE => {
                let invocation: Invocation =
                    serde_json::from_value(request["params"].clone()).unwrap();
                match WriteFileTool.prepare(&context, invocation.arguments).await {
                    Ok(Some(plan)) => {
                        sequence += 1;
                        let token = format!("opaque-plan-{sequence}");
                        let preview = plan.preview();
                        plans.insert(token.clone(), plan);
                        Ok(serde_json::to_value(Prepared { token, preview }).unwrap())
                    }
                    Ok(None) => Err(ToolFailure::new("fixture", "missing prepared plan")),
                    Err(error) => Err(error),
                }
            }
            DISPOSE => {
                let plan: Plan = serde_json::from_value(request["params"].clone()).unwrap();
                plans.remove(&plan.token);
                Ok(serde_json::to_value(Disposed { disposed: true }).unwrap())
            }
            COMMIT => {
                let plan: Plan = serde_json::from_value(request["params"].clone()).unwrap();
                match plans.remove(&plan.token) {
                    Some(plan) => plan.execute(&context).await.map(|s| {
                        serde_json::to_value(Completed {
                            output: s.output,
                            truncated: s.truncated,
                        })
                        .unwrap()
                    }),
                    None => Err(ToolFailure::new("missing_plan", "plan not found")),
                }
            }
            CALL => {
                let invocation: Invocation =
                    serde_json::from_value(request["params"].clone()).unwrap();
                if mode == "descendant" {
                    let child = std::process::Command::new("/bin/sleep")
                        .arg("60")
                        .spawn()
                        .unwrap();
                    std::fs::write(workspace.join("descendant.pid"), child.id().to_string())
                        .unwrap();
                    std::mem::forget(child);
                }
                ReadFileTool
                    .execute(&context, invocation.arguments)
                    .await
                    .map(|s| {
                        serde_json::to_value(Completed {
                            output: s.output,
                            truncated: s.truncated,
                        })
                        .unwrap()
                    })
            }
            _ => Err(ToolFailure::new("unknown_method", "unknown method")),
        };
        match result {
            Ok(value) => {
                reply(id, value.clone());
                if mode == "duplicate" {
                    reply(id, value);
                }
            }
            Err(error) => failed(id, -32010, &error.message),
        }
    }
}

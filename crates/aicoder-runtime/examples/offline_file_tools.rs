//! Real read-only discovery/search/read pages through the shared tool dispatcher.
use aicoder_core::{
    model::ModelToolCall,
    tools::{DenyAllApproval, DispatcherConfig, ToolDispatcher},
};
use aicoder_runtime::tools::default_registry;
use anyhow::{Result, ensure};
use serde_json::{Value, json};
use std::{fs, sync::Arc};

async fn call(dispatcher: &ToolDispatcher, name: &str, arguments: Value) -> Result<Value> {
    let messages = dispatcher
        .dispatch(&[ModelToolCall {
            id: name.into(),
            name: name.into(),
            arguments,
        }])
        .await?;
    let (_, result, _) = messages[0].tool_result().expect("tool result");
    ensure!(result["ok"] == true, "{result}");
    ensure!(
        result["output"].is_object(),
        "structured pagination must survive dispatch"
    );
    Ok(result["output"].clone())
}
#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let workspace = tempfile::tempdir()?;
    fs::create_dir(workspace.path().join("src"))?;
    fs::write(
        workspace.path().join("src/a.rs"),
        "// needle alpha\n// needle beta\n",
    )?;
    fs::write(workspace.path().join("src/b.rs"), "// unrelated\n")?;
    fs::write(workspace.path().join("excluded.rs"), "// needle excluded\n")?;
    fs::write(workspace.path().join(".gitignore"), "excluded.rs\n")?;
    let dispatcher = ToolDispatcher::new(
        Arc::new(default_registry()?),
        workspace.path(),
        Arc::new(DenyAllApproval),
        DispatcherConfig {
            max_output_bytes: 4096,
            ..Default::default()
        },
    )?;
    let listing = call(&dispatcher, "list_directory", json!({})).await?;
    ensure!(listing["entries"][0]["path"] == "src");
    let mut args = json!({"pattern":"*.rs","limit":1});
    let mut files = Vec::new();
    loop {
        let output = call(&dispatcher, "glob", args.clone()).await?;
        for entry in output["entries"].as_array().expect("entries") {
            files.push(entry["path"].as_str().expect("path").to_string());
        }
        if output["has_more"] == false {
            break;
        }
        args["cursor"] = output["next_cursor"].clone();
    }
    ensure!(files == ["src/a.rs", "src/b.rs"]);
    let first = call(
        &dispatcher,
        "grep",
        json!({"pattern":"needle","max_results":1}),
    )
    .await?;
    let second = call(
        &dispatcher,
        "grep",
        json!({"pattern":"needle","max_results":1,"cursor":first["next_cursor"]}),
    )
    .await?;
    ensure!(first["records"][0]["line"] == 1 && second["records"][0]["line"] == 2);
    let record = &second["records"][0];
    let read = call(
        &dispatcher,
        "read_file",
        json!({"path":record["path"],"cursor":record["read_cursor"],"limit_lines":1}),
    )
    .await?;
    ensure!(read["content"] == "// needle beta");
    println!(
        "{}",
        json!({"status":"completed","files":files,"matched_lines":2,"read":read["content"],"approval":"deny_all_read_only"})
    );
    Ok(())
}

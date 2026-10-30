//! Real canonical JSON/HTML export and explicit directory publication, no model or credentials.
use aicoder_core::{
    TurnExecutionContext,
    model::{ContentBlock, Message, MessageRole, ModelToolCall},
    session::*,
};
use aicoder_runtime::{exports::*, session::JsonlSessionRepository};
use serde_json::json;
#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let workspace = tempfile::tempdir()?;
    let storage = tempfile::tempdir()?;
    let temporary = tempfile::tempdir()?;
    let output = std::env::args_os()
        .nth(1)
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| temporary.path().into());
    std::fs::create_dir_all(&output)?;
    let repo = JsonlSessionRepository::new(storage.path())?;
    let mut session = repo.create(workspace.path())?;
    repo.append_all(
        &mut session,
        [
            Message::text(
                MessageRole::User,
                "Inspect this safely: </pre><script>window.injected=true</script>",
            ),
            Message {
                role: MessageRole::Assistant,
                content: vec![ContentBlock::ToolCall {
                    call: ModelToolCall {
                        id: "read-1".into(),
                        name: "read_file".into(),
                        arguments: json!({"path":"example.rs"}),
                    },
                }],
            },
            Message {
                role: MessageRole::Tool,
                content: vec![ContentBlock::ToolResult {
                    call_id: "read-1".into(),
                    content: json!({"text":"<img src='https://remote.invalid/x'>"}),
                    is_error: false,
                }],
            },
            Message::text(
                MessageRole::Assistant,
                "Recorded result remains inspectable.",
            ),
        ],
    )?;
    SessionNavigator::new(&repo, workspace.path())?.execute(
        &session.metadata().id,
        ArchiveNavigation::Branch {
            source: BranchSource::Root,
            name: Some("retained alternative".into()),
        },
    )?;
    session = repo.open(&session.metadata().id)?;
    repo.append(
        &mut session,
        Message::text(
            MessageRole::User,
            "Current branch; older branch remains in this export.",
        ),
    )?;
    let context = TurnExecutionContext::new();
    let json = prepare(
        &repo,
        workspace.path(),
        &session.metadata().id,
        ExportFormat::Json,
        ExportLimits::default(),
        &context,
    )?;
    let html = prepare(
        &repo,
        workspace.path(),
        &session.metadata().id,
        ExportFormat::Html,
        ExportLimits::default(),
        &context,
    )?;
    let json_receipt = json.write_new(&output.join("session.json"), &context)?;
    let html_receipt = html.write_new(&output.join("session.html"), &context)?;
    let shared = output.join("shared");
    std::fs::create_dir_all(&shared)?;
    let publisher = DirectoryPublisher::new(&shared, Some("https://example.invalid/exports/"))?;
    let published = publisher.publish(&html, context).await?;
    anyhow::ensure!(
        std::fs::read(&published.path)? == html.bytes(),
        "publisher changed prepared bytes"
    );
    println!(
        "{}",
        json!({"json":json_receipt,"html":html_receipt,"publication":published,"provider_calls":0})
    );
    Ok(())
}

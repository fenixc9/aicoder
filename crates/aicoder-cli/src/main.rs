use std::{
    io::{self, Write},
    path::PathBuf,
    sync::Arc,
};

use aicoder_core::{
    Agent, AgentConfig, AgentTypeEventHandler, ChatClient,
    events::{
        ContentChunkEvent, ContentEndedEvent, ContentStartedEvent, ReasoningChunkEvent,
        ReasoningEndedEvent, ReasoningStartedEvent, ToolCallEndedEvent, ToolExecutionEndedEvent,
    },
    tools::{AllowAllApproval, ApprovalHandler, ToolInvocation},
    types::{ChatCompletionRequest, ChatMessage, Role},
};
use anyhow::{Context, Result};
use async_trait::async_trait;
use clap::Parser;

#[derive(Debug, Parser)]
#[command(name = "aicoder", about = "A small tool-using coding agent")]
struct Cli {
    /// User prompt sent to the model.
    #[arg(short, long, default_value = "你是谁")]
    prompt: String,

    /// Automatically approve mutating tools and commands.
    #[arg(long)]
    yes: bool,

    /// Workspace available to file and command tools.
    #[arg(long, default_value = ".")]
    workspace: PathBuf,
}

struct ConsoleEvents;

struct ConsoleApproval;

#[async_trait]
impl ApprovalHandler for ConsoleApproval {
    async fn approve(&self, invocation: &ToolInvocation) -> Result<bool> {
        let invocation = invocation.clone();
        tokio::task::spawn_blocking(move || {
            eprintln!(
                "\n工具 {} 将以当前用户权限执行（不是沙箱）\n参数: {}",
                invocation.name,
                serde_json::to_string_pretty(&invocation.arguments)?
            );
            eprint!("允许执行? [y/N] ");
            io::stderr().flush()?;

            let mut input = String::new();
            io::stdin().read_line(&mut input)?;
            Ok(matches!(
                input.trim().to_ascii_lowercase().as_str(),
                "y" | "yes"
            ))
        })
        .await
        .context("Approval prompt task failed")?
    }
}

impl AgentTypeEventHandler for ConsoleEvents {
    fn on_tool_call_ended(&self, event: ToolCallEndedEvent) {
        println!("[Tool]{:?}", event.tool_call);
    }

    fn on_tool_execution_ended(&self, event: ToolExecutionEndedEvent) {
        println!("[ToolExec]{:?}", event.name);
    }

    fn on_reasoning_started(&self, _event: ReasoningStartedEvent) {
        print!("[Reasoning]:");
        flush_stdout();
    }

    fn on_reasoning_chunk(&self, event: ReasoningChunkEvent) {
        print!("{}", event.delta);
        flush_stdout();
    }

    fn on_reasoning_ended(&self, _event: ReasoningEndedEvent) {
        println!();
    }

    fn on_content_started(&self, _event: ContentStartedEvent) {
        println!();
        print!("[Content]:");
        flush_stdout();
    }

    fn on_content_chunk(&self, event: ContentChunkEvent) {
        print!("{}", event.delta);
        flush_stdout();
    }

    fn on_content_ended(&self, _event: ContentEndedEvent) {
        println!();
    }
}

fn flush_stdout() {
    let _ = std::io::stdout().flush();
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_target(false)
        .with_level(true)
        .init();
    let cli = Cli::parse();

    let model = std::env::var("OPENAI_MODEL").unwrap_or_else(|_| "deepseek-v4-flash".to_string());
    let client = ChatClient::from_env(&model)?;
    let builder = Agent::builder(client)
        .workspace(&cli.workspace)
        .config(AgentConfig::default());
    let agent = if cli.yes {
        builder.approval(AllowAllApproval).build()?
    } else {
        builder.approval(ConsoleApproval).build()?
    };

    let request = ChatCompletionRequest {
        model,
        messages: vec![
            ChatMessage {
                role: Role::System,
                content: Some(
                    "You are a helpful coding assistant. Reply in Chinese. Use tools when needed."
                        .to_string(),
                ),
                reasoning: None,
                tool_calls: None,
                tool_call_id: None,
                name: None,
            },
            ChatMessage {
                role: Role::User,
                content: Some(cli.prompt),
                reasoning: None,
                tool_calls: None,
                tool_call_id: None,
                name: None,
            },
        ],
        temperature: Some(0.7),
        top_p: Some(1.0),
        max_tokens: Some(2048),
        seed: None,
        tools: None,
        tool_choice: None,
        stream: Some(true),
        stop: None,
        response_format: None,
    };

    let result = agent
        .run_with_handler(request, Arc::new(ConsoleEvents))
        .await?;
    if let Some(reasoning) = &result.final_message.reasoning {
        println!("=== 推理过程 ===\n{reasoning}\n");
    }
    if let Some(content) = &result.final_message.content {
        println!("=== 回复 ===\n{content}");
    } else {
        println!("(空回复)");
    }
    println!(
        "\n=== 使用量 ===\n轮次: {}\n消息: {}\n提示词: {} tokens\n缓存命中: {} tokens\n未缓存: {} tokens\n缓存命中率: {:.2}%\n生成: {} tokens\n总计: {} tokens",
        result.rounds,
        result.messages.len(),
        result.usage.prompt_tokens,
        result.usage.cached_tokens(),
        result.usage.uncached_tokens(),
        result.usage.cache_hit_rate() * 100.0,
        result.usage.completion_tokens,
        result.usage.total_tokens
    );
    if let Some(cache_write_tokens) = result
        .usage
        .prompt_tokens_details
        .as_ref()
        .and_then(|details| details.cache_write_tokens)
    {
        println!("缓存写入: {cache_write_tokens} tokens");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_accepts_prompt() {
        let cli = Cli::try_parse_from(["aicoder", "--prompt", "检查当前项目"]).unwrap();
        assert_eq!(cli.prompt, "检查当前项目");
        assert!(!cli.yes);
        assert_eq!(cli.workspace, PathBuf::from("."));
    }

    #[test]
    fn cli_keeps_default_prompt() {
        let cli = Cli::try_parse_from(["aicoder"]).unwrap();
        assert_eq!(cli.prompt, "你是谁");
    }
}

mod agent;
mod client;
mod events;
mod redaction;
mod tools;
mod types;

use std::{path::PathBuf, sync::Arc};

use anyhow::Result;
use clap::Parser;

use agent::{Agent, AgentConfig};
use tools::{
    AllowAllApproval, ApprovalHandler, ConsoleApproval, DispatcherConfig, ToolDispatcher,
    default_registry,
};

use crate::events::{
    AgentTypeEventHandler, ContentChunkEvent, ContentEndedEvent, ContentStartedEvent,
    ReasoningChunkEvent, ReasoningEndedEvent, ReasoningStartedEvent, ToolCallStartedEvent,
};

#[derive(Debug, Parser)]
#[command(name = "aicoder", about = "A small tool-using coding agent")]
struct Cli {
    /// User prompt sent to the model.
    #[arg(short, long, default_value = "你是谁")]
    prompt: String,

    /// Automatically approve write_file and bash calls.
    #[arg(long)]
    yes: bool,

    /// Workspace available to file and command tools.
    #[arg(long, default_value = ".")]
    workspace: PathBuf,
}

struct AppHandler {}
#[allow(dead_code)]
type OnToolCallStartedFn = fn();

impl AgentTypeEventHandler for AppHandler {
    fn on_tool_call_started(&self, ev: ToolCallStartedEvent) {
        // print!("[Tool]:")
    }

    fn on_tool_call_chunk(&self, ev: events::ToolCallChunkEvent) {
        // print!("{:?}", ev.name_delta) 
    }

    fn on_tool_call_ended(&self, ev: events::ToolCallEndedEvent) {
        println!("[Tool]{:?}", ev.tool_call)
    }

    
    fn on_tool_execution_ended(&self, ev: events::ToolExecutionEndedEvent) {
        println!("[ToolExec]{:?}", ev.name)
    }



    fn on_reasoning_started(&self, _event: ReasoningStartedEvent) {
        // tracing::info!(
        //     sequence = event.meta.sequence,
        //     choice_index = event.choice_index,
        //     "reasoning started"
        // );
        print!("[Reasoning]:")
    }

    fn on_reasoning_chunk(&self, event: ReasoningChunkEvent) {
        // tracing::info!(
        //     sequence = event.meta.sequence,
        //     choice_index = event.choice_index,
        //     delta = event.delta,
        //     "reasoning chunk"
        // );
        //
        print!("{}", event.delta)
    }

    fn on_reasoning_ended(&self, _event: ReasoningEndedEvent) {
        println!()
    }

    fn on_content_started(&self, _event: ContentStartedEvent) {
        println!();
        print!("[Content]:");
    }

    fn on_content_chunk(&self, event: ContentChunkEvent) {
        print!("{}", event.delta)
    }

    fn on_content_ended(&self, _event: ContentEndedEvent) {
        println!()
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_target(false)
        .with_level(true)
        .init();
    let cli = Cli::parse();

    let model = std::env::var("OPENAI_MODEL").unwrap_or_else(|_| "deepseek-v4-flash".to_string());
    let client = Arc::new(client::ChatClient::from_env(&model)?);

    let registry = Arc::new(default_registry()?);
    let approval: Arc<dyn ApprovalHandler> = if cli.yes {
        Arc::new(AllowAllApproval)
    } else {
        Arc::new(ConsoleApproval)
    };
    let dispatcher = Arc::new(ToolDispatcher::new(
        registry,
        &cli.workspace,
        approval,
        DispatcherConfig::default(),
    )?);
    let agent = Agent::new(client, dispatcher, AgentConfig::default());

    let request = types::ChatCompletionRequest {
        model,
        messages: vec![
            types::ChatMessage {
                role: types::Role::System,
                content: Some(
                    "You are a helpful coding assistant. Reply in Chinese. Use tools when needed."
                        .to_string(),
                ),
                reasoning: None,
                tool_calls: None,
                tool_call_id: None,
                name: None,
            },
            types::ChatMessage {
                role: types::Role::User,
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
        .run_with_type_handler(request, Arc::new(AppHandler {}))
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

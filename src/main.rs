mod client;
mod types;

#[tokio::main]
async fn main() {
    // 初始化日志
    tracing_subscriber::fmt()
        .with_target(false)
        .with_level(true)
        .init();

    // 从环境变量读取 API key
    let api_key = std::env::var("OPENAI_API_KEY")
        .or_else(|_| std::env::var("ANTHROPIC_API_KEY"))
        .or_else(|_| std::env::var("DASHSCOPE_API_KEY"))
        .or_else(|_| std::env::var("ZHIPU_API_KEY"))
        .unwrap_or_else(|_| {
            eprintln!("⚠️  未设置 API key，请先设置 OPENAI_API_KEY 或其他环境变量");
            eprintln!("  参考 .env.example 创建 .env 文件，或在运行时传入环境变量");
            std::process::exit(1);
        });

    let config = client::ClientConfig {
        base_url: std::env::var("OPENAI_API_BASE")
            .unwrap_or_else(|_| "https://api.openai.com/v1".to_string()),
        api_key,
        model: "/models/unsloth_Qwen3.6-35B-A3B-NVFP4-Fast".to_string(),
        timeout: 120,
        max_retries: 3,
    };
    let client = client::ChatClient::new(config)
        .expect("Failed to create client");

    // 构建请求
    let request = types::ChatCompletionRequest {
        model: "/models/unsloth_Qwen3.6-35B-A3B-NVFP4-Fast".to_string(),
        messages: vec![
            types::ChatMessage {
                role: types::Role::System,
                content: Some("You are a helpful coding assistant. Reply in Chinese."
                    .to_string()),
                reasoning: None,
                tool_calls: None,
                tool_call_id: None,
                name: None,
            },
            types::ChatMessage {
                role: types::Role::User,
                content: Some("用 Rust 写一个快速排序".to_string()),
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
        stream: Some(false),
        stop: None,
        response_format: None,
    };

    // 调用 API
    match client.chat_completion(request).await {
        Ok(response) => {
            println!("=== 响应 ===");
            if let Some(choice) = response.choices.first() {
                if let Some(reasoning) = &choice.message.reasoning {
                    println!("=== 推理过程 ===\n{}\n", reasoning);
                }
                if let Some(content) = &choice.message.content {
                    println!("=== 回复 ===\n{}", content);
                } else if choice.message.reasoning.is_none() {
                    println!("(空回复)");
                }
                if let Some(ref tool_calls) = choice.message.tool_calls {
                    for tc in tool_calls {
                        println!(
                            "  工具调用: {}({})",
                            tc.function.name, tc.function.arguments
                        );
                    }
                }
            }
            if let Some(usage) = &response.usage {
                println!("\n=== 使用量 ===");
                println!("提示词: {} tokens", usage.prompt_tokens);
                println!("生成: {} tokens", usage.completion_tokens);
                println!("总计: {} tokens", usage.total_tokens);
            }
        }
        Err(e) => {
            eprintln!("请求失败: {}", e);
        }
    }
}

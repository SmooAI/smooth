# smooth-operator

Rust-native AI agent framework with built-in checkpointing, tool system, and LLM client. Purpose-built for orchestrated agent workloads with security-first design.

## Features

- **LLM Client** -- OpenAI-compatible and Anthropic API support with streaming, retry policies, and rate limit handling
- **Tool System** -- Trait-based tools with pre/post hooks for surveillance, secret detection, and prompt injection guards
- **Agent Loop** -- Observe-think-act cycle with configurable iteration limits, parallel tool execution, and cost budgets
- **Checkpointing** -- Pluggable checkpoint stores for session resume and fault tolerance
- **Conversation Management** -- Token-aware context window with compaction strategies
- **Memory & Knowledge** -- Trait-based memory and knowledge base integration
- **Cost Tracking** -- Per-model pricing, token budgets, and budget enforcement

## Quick Start

```rust
use smooth_operator::{Agent, AgentConfig, LlmConfig, Tool, ToolRegistry, ToolSchema};
use async_trait::async_trait;

// Define a tool
struct GetWeather;

#[async_trait]
impl Tool for GetWeather {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "get_weather".into(),
            description: "Get current weather for a city".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "city": { "type": "string", "description": "City name" }
                },
                "required": ["city"]
            }),
        }
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<String> {
        let city = args["city"].as_str().unwrap_or("unknown");
        Ok(format!("Weather in {city}: 72F, sunny"))
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Configure LLM
    let llm = LlmConfig {
        api_url: "https://api.openai.com/v1".into(),
        api_key: std::env::var("OPENAI_API_KEY")?,
        model: "gpt-4o".into(),
        ..Default::default()
    };

    // Build agent with tools
    let config = AgentConfig::new("assistant", "You are a helpful assistant.", llm)
        .with_max_iterations(10)
        .with_parallel_tools(true);

    let mut registry = ToolRegistry::new();
    registry.register(GetWeather);

    let mut agent = Agent::new(config, registry);
    let events = agent.run("What's the weather in Tokyo?").await?;

    for event in events {
        println!("{event:?}");
    }

    Ok(())
}
```

## License

MIT

## Links

- [GitHub](https://github.com/SmooAI/smooth)
- [crates.io](https://crates.io/crates/smooth-operator)

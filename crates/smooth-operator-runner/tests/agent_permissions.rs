//! Integration test: the PermissionHook blocks denied tool calls
//! before they execute.
//!
//! This is the load-bearing guarantee the primary-agents pearl adds:
//! a `plan`-mode agent that tries to call `edit_file` gets a tool-
//! result error "agent 'plan' is not permitted to call 'edit_file'"
//! — NOT a prompt-level refusal. The test registers a fake
//! `edit_file` tool that would crash the test if it actually ran,
//! and asserts the hook intercepts the call before execution.

use async_trait::async_trait;
use serde_json::json;
use smooth_operator::agents::AgentRegistry;
use smooth_operator::tool::{Tool, ToolCall, ToolRegistry, ToolSchema};
use smooth_operator::PermissionHook;

/// Fake `edit_file` that panics if called. If the PermissionHook
/// lets the call through, this panic is how we find out.
struct PanickingEditTool;

#[async_trait]
impl Tool for PanickingEditTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "edit_file".into(),
            description: "fake edit tool that must never execute under plan/think/review".into(),
            parameters: json!({"type": "object"}),
        }
    }

    async fn execute(&self, _arguments: serde_json::Value) -> anyhow::Result<String> {
        panic!("PermissionHook did not block edit_file — fake tool was executed");
    }
}

/// Benign `read_file` that echoes the requested path. Used to
/// prove the hook is not blocking *every* tool, just the denied
/// ones.
struct EchoReadTool;

#[async_trait]
impl Tool for EchoReadTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "read_file".into(),
            description: "echoing read tool — always returns 'ok'".into(),
            parameters: json!({"type": "object"}),
        }
    }

    async fn execute(&self, _arguments: serde_json::Value) -> anyhow::Result<String> {
        Ok("ok".into())
    }
}

#[tokio::test]
async fn plan_agent_blocks_edit_file_at_dispatch() {
    let registry = AgentRegistry::builtin();
    let plan = registry.get("plan").expect("'plan' must be registered");

    let mut tools = ToolRegistry::new();
    tools.register(PanickingEditTool);
    tools.register(EchoReadTool);
    tools.add_hook(PermissionHook::new(plan));

    let call = ToolCall {
        id: "call-edit".into(),
        name: "edit_file".into(),
        arguments: json!({"path": "src/lib.rs", "content": "bad"}),
    };
    let result = tools.execute(&call).await;

    assert!(result.is_error, "plan-mode edit_file must be marked as error");
    assert!(
        result.content.contains("agent 'plan' is not permitted to call 'edit_file'"),
        "expected permission block message, got: {}",
        result.content
    );
    // The block happens in pre_call before the tool runs, so the
    // tool_call_id is preserved and the content starts with the
    // hook's "blocked by hook:" prefix (registry-added) + our message.
    assert_eq!(result.tool_call_id, "call-edit");
}

#[tokio::test]
async fn plan_agent_allows_read_file_at_dispatch() {
    let registry = AgentRegistry::builtin();
    let plan = registry.get("plan").expect("'plan' must be registered");

    let mut tools = ToolRegistry::new();
    tools.register(PanickingEditTool);
    tools.register(EchoReadTool);
    tools.add_hook(PermissionHook::new(plan));

    let call = ToolCall {
        id: "call-read".into(),
        name: "read_file".into(),
        arguments: json!({"path": "README.md"}),
    };
    let result = tools.execute(&call).await;

    assert!(!result.is_error, "plan-mode read_file must succeed, got: {}", result.content);
    assert_eq!(result.content, "ok");
}

#[tokio::test]
async fn code_agent_allows_edit_file_at_dispatch() {
    // Fake code agent with an OK edit tool so we don't trigger the
    // panicking one — this test proves the hook doesn't over-block.
    struct OkEditTool;
    #[async_trait]
    impl Tool for OkEditTool {
        fn schema(&self) -> ToolSchema {
            ToolSchema {
                name: "edit_file".into(),
                description: "ok".into(),
                parameters: json!({"type": "object"}),
            }
        }
        async fn execute(&self, _arguments: serde_json::Value) -> anyhow::Result<String> {
            Ok("edited".into())
        }
    }

    let registry = AgentRegistry::builtin();
    let code = registry.get("code").expect("'code' must be registered");

    let mut tools = ToolRegistry::new();
    tools.register(OkEditTool);
    tools.add_hook(PermissionHook::new(code));

    let call = ToolCall {
        id: "call-edit-ok".into(),
        name: "edit_file".into(),
        arguments: json!({}),
    };
    let result = tools.execute(&call).await;

    assert!(!result.is_error, "code agent must be allowed to edit_file: {}", result.content);
    assert_eq!(result.content, "edited");
}

#[tokio::test]
async fn think_agent_blocks_bash_at_dispatch() {
    struct PanickingBashTool;
    #[async_trait]
    impl Tool for PanickingBashTool {
        fn schema(&self) -> ToolSchema {
            ToolSchema {
                name: "bash".into(),
                description: "fake bash".into(),
                parameters: json!({"type": "object"}),
            }
        }
        async fn execute(&self, _arguments: serde_json::Value) -> anyhow::Result<String> {
            panic!("PermissionHook did not block bash for think agent");
        }
    }

    let registry = AgentRegistry::builtin();
    let think = registry.get("think").expect("'think' must be registered");

    let mut tools = ToolRegistry::new();
    tools.register(PanickingBashTool);
    tools.add_hook(PermissionHook::new(think));

    let call = ToolCall {
        id: "call-bash".into(),
        name: "bash".into(),
        arguments: json!({"command": "rm -rf /"}),
    };
    let result = tools.execute(&call).await;

    assert!(result.is_error);
    assert!(
        result.content.contains("agent 'think' is not permitted to call 'bash'"),
        "expected permission block, got: {}",
        result.content
    );
}

//! REST API endpoints for operator tool invocation.
//!
//! Replaces MCP with direct HTTP calls. Operators call these endpoints
//! via the Goalie proxy, authenticated by their operator token.

use std::sync::Arc;
use std::time::Instant;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::Json;
use axum::routing::{get, post};
use axum::Router;
use serde::{Deserialize, Serialize};
use smooth_operator::tool::{ToolCall, ToolRegistry};

// ── Types ─────────────────────────────────────────────────

/// Describes a tool available for invocation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolInfo {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

/// Request body for POST /api/tools/invoke.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InvokeRequest {
    pub tool_name: String,
    pub arguments: serde_json::Value,
    pub operator_id: String,
}

/// Response from a single tool invocation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InvokeResponse {
    pub result: String,
    pub is_error: bool,
    pub duration_ms: u64,
}

/// A single call within a batch request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchCall {
    pub tool_name: String,
    pub arguments: serde_json::Value,
}

/// Request body for POST /api/tools/batch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchRequest {
    pub calls: Vec<BatchCall>,
    pub operator_id: String,
}

/// Response item for a batch invocation, including tool name.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchResponseItem {
    pub tool_name: String,
    pub result: String,
    pub is_error: bool,
    pub duration_ms: u64,
}

/// Response from POST /api/tools/batch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchResponse {
    pub results: Vec<BatchResponseItem>,
}

// ── Shared state ──────────────────────────────────────────

/// State shared across tool API routes.
#[derive(Clone)]
pub struct ToolApiState {
    pub registry: Arc<ToolRegistry>,
}

// ── Handlers ──────────────────────────────────────────────

/// GET /api/tools — list available tools with their JSON schemas.
async fn list_tools(State(state): State<ToolApiState>) -> Json<Vec<ToolInfo>> {
    let schemas = state.registry.schemas();
    let tools = schemas
        .into_iter()
        .map(|s| ToolInfo {
            name: s.name,
            description: s.description,
            parameters: s.parameters,
        })
        .collect();
    Json(tools)
}

/// GET /api/tools/:name/schema — get schema for a specific tool.
async fn get_tool_schema(State(state): State<ToolApiState>, Path(name): Path<String>) -> Result<Json<ToolInfo>, StatusCode> {
    let schemas = state.registry.schemas();
    schemas
        .into_iter()
        .find(|s| s.name == name)
        .map(|s| {
            Json(ToolInfo {
                name: s.name,
                description: s.description,
                parameters: s.parameters,
            })
        })
        .ok_or(StatusCode::NOT_FOUND)
}

/// POST /api/tools/invoke — invoke a single tool.
async fn invoke_tool(State(state): State<ToolApiState>, Json(req): Json<InvokeRequest>) -> Result<Json<InvokeResponse>, StatusCode> {
    if !state.registry.has_tool(&req.tool_name) {
        return Ok(Json(InvokeResponse {
            result: format!("unknown tool: {}", req.tool_name),
            is_error: true,
            duration_ms: 0,
        }));
    }

    let call = ToolCall {
        id: format!("{}-{}", req.operator_id, uuid::Uuid::new_v4()),
        name: req.tool_name,
        arguments: req.arguments,
    };

    let start = Instant::now();
    let tool_result = state.registry.execute(&call).await;
    let duration_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);

    Ok(Json(InvokeResponse {
        result: tool_result.content,
        is_error: tool_result.is_error,
        duration_ms,
    }))
}

/// POST /api/tools/batch — invoke multiple tools in parallel.
async fn batch_invoke(State(state): State<ToolApiState>, Json(req): Json<BatchRequest>) -> Json<BatchResponse> {
    let futures: Vec<_> = req
        .calls
        .into_iter()
        .map(|batch_call| {
            let registry = Arc::clone(&state.registry);
            let operator_id = req.operator_id.clone();
            tokio::spawn(async move {
                let call = ToolCall {
                    id: format!("{}-{}", operator_id, uuid::Uuid::new_v4()),
                    name: batch_call.tool_name.clone(),
                    arguments: batch_call.arguments,
                };
                let start = Instant::now();
                let tool_result = registry.execute(&call).await;
                let duration_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);
                BatchResponseItem {
                    tool_name: batch_call.tool_name,
                    result: tool_result.content,
                    is_error: tool_result.is_error,
                    duration_ms,
                }
            })
        })
        .collect();

    let mut results = Vec::with_capacity(futures.len());
    for handle in futures {
        match handle.await {
            Ok(item) => results.push(item),
            Err(e) => results.push(BatchResponseItem {
                tool_name: String::new(),
                result: format!("task join error: {e}"),
                is_error: true,
                duration_ms: 0,
            }),
        }
    }

    Json(BatchResponse { results })
}

// ── Router builder ────────────────────────────────────────

/// Build the tool API router for nesting into the main server.
pub fn build_tool_api_router(registry: Arc<ToolRegistry>) -> Router {
    let state = ToolApiState { registry };
    Router::new()
        .route("/api/tools", get(list_tools))
        .route("/api/tools/invoke", post(invoke_tool))
        .route("/api/tools/batch", post(batch_invoke))
        .route("/api/tools/{name}/schema", get(get_tool_schema))
        .with_state(state)
}

// ── Tests ─────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use smooth_operator::tool::{Tool, ToolRegistry, ToolSchema};
    use tower::ServiceExt;

    use super::*;

    /// Echo tool for testing — echoes the "text" argument.
    struct EchoTool;

    #[async_trait::async_trait]
    impl Tool for EchoTool {
        fn schema(&self) -> ToolSchema {
            ToolSchema {
                name: "echo".into(),
                description: "Echoes back the text argument".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "text": {"type": "string"}
                    },
                    "required": ["text"]
                }),
            }
        }

        async fn execute(&self, arguments: serde_json::Value) -> anyhow::Result<String> {
            Ok(arguments["text"].as_str().unwrap_or("").to_string())
        }
    }

    fn make_registry() -> Arc<ToolRegistry> {
        let mut registry = ToolRegistry::new();
        registry.register(EchoTool);
        Arc::new(registry)
    }

    fn make_app() -> Router {
        build_tool_api_router(make_registry())
    }

    // ── Serialization tests ────────────────────────────────

    #[test]
    fn tool_info_serialization() {
        let info = ToolInfo {
            name: "echo".into(),
            description: "Echoes back".into(),
            parameters: serde_json::json!({"type": "object"}),
        };
        let json = serde_json::to_string(&info).expect("serialize ToolInfo");
        let parsed: ToolInfo = serde_json::from_str(&json).expect("deserialize ToolInfo");
        assert_eq!(parsed.name, "echo");
        assert_eq!(parsed.description, "Echoes back");
    }

    #[test]
    fn invoke_request_response_serialization() {
        let req = InvokeRequest {
            tool_name: "echo".into(),
            arguments: serde_json::json!({"text": "hi"}),
            operator_id: "op-1".into(),
        };
        let json = serde_json::to_string(&req).expect("serialize InvokeRequest");
        let parsed: InvokeRequest = serde_json::from_str(&json).expect("deserialize InvokeRequest");
        assert_eq!(parsed.tool_name, "echo");
        assert_eq!(parsed.operator_id, "op-1");

        let resp = InvokeResponse {
            result: "hi".into(),
            is_error: false,
            duration_ms: 42,
        };
        let json = serde_json::to_string(&resp).expect("serialize InvokeResponse");
        assert!(json.contains("\"is_error\":false"));
        assert!(json.contains("\"duration_ms\":42"));
    }

    #[test]
    fn batch_request_serialization() {
        let req = BatchRequest {
            calls: vec![
                BatchCall {
                    tool_name: "echo".into(),
                    arguments: serde_json::json!({"text": "a"}),
                },
                BatchCall {
                    tool_name: "echo".into(),
                    arguments: serde_json::json!({"text": "b"}),
                },
            ],
            operator_id: "op-1".into(),
        };
        let json = serde_json::to_string(&req).expect("serialize BatchRequest");
        let parsed: BatchRequest = serde_json::from_str(&json).expect("deserialize BatchRequest");
        assert_eq!(parsed.calls.len(), 2);
        assert_eq!(parsed.operator_id, "op-1");
    }

    // ── HTTP tests ─────────────────────────────────────────

    #[tokio::test]
    async fn get_tools_returns_tool_list() {
        let app = make_app();
        let req = Request::builder().uri("/api/tools").body(Body::empty()).expect("build request");
        let resp = app.oneshot(req).await.expect("send request");
        assert_eq!(resp.status(), StatusCode::OK);

        let body = axum::body::to_bytes(resp.into_body(), usize::MAX).await.expect("read body");
        let tools: Vec<ToolInfo> = serde_json::from_slice(&body).expect("parse response");
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "echo");
    }

    #[tokio::test]
    async fn get_tool_schema_returns_specific_tool() {
        let app = make_app();
        let req = Request::builder().uri("/api/tools/echo/schema").body(Body::empty()).expect("build request");
        let resp = app.oneshot(req).await.expect("send request");
        assert_eq!(resp.status(), StatusCode::OK);

        let body = axum::body::to_bytes(resp.into_body(), usize::MAX).await.expect("read body");
        let tool: ToolInfo = serde_json::from_slice(&body).expect("parse response");
        assert_eq!(tool.name, "echo");
        assert_eq!(tool.description, "Echoes back the text argument");
    }

    #[tokio::test]
    async fn get_tool_schema_returns_404_for_unknown() {
        let app = make_app();
        let req = Request::builder()
            .uri("/api/tools/nonexistent/schema")
            .body(Body::empty())
            .expect("build request");
        let resp = app.oneshot(req).await.expect("send request");
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn invoke_executes_tool() {
        let app = make_app();
        let body = serde_json::to_string(&InvokeRequest {
            tool_name: "echo".into(),
            arguments: serde_json::json!({"text": "hello world"}),
            operator_id: "op-1".into(),
        })
        .expect("serialize body");

        let req = Request::builder()
            .method("POST")
            .uri("/api/tools/invoke")
            .header("content-type", "application/json")
            .body(Body::from(body))
            .expect("build request");

        let resp = app.oneshot(req).await.expect("send request");
        assert_eq!(resp.status(), StatusCode::OK);

        let body = axum::body::to_bytes(resp.into_body(), usize::MAX).await.expect("read body");
        let invoke_resp: InvokeResponse = serde_json::from_slice(&body).expect("parse response");
        assert!(!invoke_resp.is_error);
        assert_eq!(invoke_resp.result, "hello world");
        assert!(invoke_resp.duration_ms < 1000);
    }

    #[tokio::test]
    async fn invoke_returns_error_for_unknown_tool() {
        let app = make_app();
        let body = serde_json::to_string(&InvokeRequest {
            tool_name: "nonexistent".into(),
            arguments: serde_json::json!({}),
            operator_id: "op-1".into(),
        })
        .expect("serialize body");

        let req = Request::builder()
            .method("POST")
            .uri("/api/tools/invoke")
            .header("content-type", "application/json")
            .body(Body::from(body))
            .expect("build request");

        let resp = app.oneshot(req).await.expect("send request");
        assert_eq!(resp.status(), StatusCode::OK);

        let body = axum::body::to_bytes(resp.into_body(), usize::MAX).await.expect("read body");
        let invoke_resp: InvokeResponse = serde_json::from_slice(&body).expect("parse response");
        assert!(invoke_resp.is_error);
        assert!(invoke_resp.result.contains("unknown tool"));
    }

    #[tokio::test]
    async fn batch_executes_multiple_tools() {
        let app = make_app();
        let body = serde_json::to_string(&BatchRequest {
            calls: vec![
                BatchCall {
                    tool_name: "echo".into(),
                    arguments: serde_json::json!({"text": "first"}),
                },
                BatchCall {
                    tool_name: "echo".into(),
                    arguments: serde_json::json!({"text": "second"}),
                },
            ],
            operator_id: "op-1".into(),
        })
        .expect("serialize body");

        let req = Request::builder()
            .method("POST")
            .uri("/api/tools/batch")
            .header("content-type", "application/json")
            .body(Body::from(body))
            .expect("build request");

        let resp = app.oneshot(req).await.expect("send request");
        assert_eq!(resp.status(), StatusCode::OK);

        let body = axum::body::to_bytes(resp.into_body(), usize::MAX).await.expect("read body");
        let batch_resp: BatchResponse = serde_json::from_slice(&body).expect("parse response");
        assert_eq!(batch_resp.results.len(), 2);

        let results: Vec<&str> = batch_resp.results.iter().map(|r| r.result.as_str()).collect();
        assert!(results.contains(&"first"));
        assert!(results.contains(&"second"));
    }

    #[tokio::test]
    async fn batch_response_contains_all_results() {
        let app = make_app();
        let body = serde_json::to_string(&BatchRequest {
            calls: vec![
                BatchCall {
                    tool_name: "echo".into(),
                    arguments: serde_json::json!({"text": "ok"}),
                },
                BatchCall {
                    tool_name: "nonexistent".into(),
                    arguments: serde_json::json!({}),
                },
            ],
            operator_id: "op-1".into(),
        })
        .expect("serialize body");

        let req = Request::builder()
            .method("POST")
            .uri("/api/tools/batch")
            .header("content-type", "application/json")
            .body(Body::from(body))
            .expect("build request");

        let resp = app.oneshot(req).await.expect("send request");
        assert_eq!(resp.status(), StatusCode::OK);

        let body = axum::body::to_bytes(resp.into_body(), usize::MAX).await.expect("read body");
        let batch_resp: BatchResponse = serde_json::from_slice(&body).expect("parse response");
        assert_eq!(batch_resp.results.len(), 2);

        // One success, one error
        let successes = batch_resp.results.iter().filter(|r| !r.is_error).count();
        let errors = batch_resp.results.iter().filter(|r| r.is_error).count();
        assert_eq!(successes, 1);
        assert_eq!(errors, 1);
    }
}

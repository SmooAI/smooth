//! `web_search` operator tool.
//!
//! Mirrors OpenCode's `tool/websearch.ts` + `tool/mcp-websearch.ts`: posts an
//! MCP JSON-RPC `tools/call` to a hosted LLM-tuned search provider that
//! returns extracted, LLM-ready text (not raw SERP HTML). Two providers
//! are supported behind the same surface so smooth-vs-opencode head-to-head
//! benches run on the same backend:
//!
//! - **Exa** (`https://mcp.exa.ai/mcp?exaApiKey=…`) — primary. Tool name
//!   `web_search_exa`, knobs `type` / `numResults` / `livecrawl` /
//!   `contextMaxCharacters`.
//! - **Parallel** (`https://search.parallel.ai/mcp`) — fallback. Tool name
//!   `web_search`, knobs `objective` / `search_queries`.
//!
//! Provider selection: if `SMOOTH_EXA_API_KEY` is set use Exa; else if
//! `SMOOTH_PARALLEL_API_KEY` is set use Parallel; else error with an
//! actionable message naming both env vars. The operator env override
//! `SMOOTH_WEBSEARCH_PROVIDER=exa|parallel` forces one.
//!
//! Egress allowlist: only `mcp.exa.ai` + `search.parallel.ai`. Wonk pearl
//! `th-bf3f6e` adds these to the default policy template.

use async_trait::async_trait;
use serde_json::json;
use smooth_operator::tool::{Tool, ToolSchema};

const EXA_HOST: &str = "https://mcp.exa.ai/mcp";
const PARALLEL_URL: &str = "https://search.parallel.ai/mcp";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Provider {
    Exa,
    Parallel,
}

impl Provider {
    fn label(self) -> &'static str {
        match self {
            Self::Exa => "exa",
            Self::Parallel => "parallel",
        }
    }
}

pub struct WebSearchTool {
    pub client: reqwest::Client,
}

/// Resolved provider config. For Exa the API key goes on the URL query
/// (`?exaApiKey=`), for Parallel as a Bearer header — `query_key` holds
/// the Exa key, `bearer` holds the Parallel key.
#[derive(Debug)]
pub(crate) struct ProviderConfig {
    pub(crate) provider: Provider,
    pub(crate) url: &'static str,
    pub(crate) query_key: Option<String>,
    pub(crate) bearer: Option<String>,
}

impl WebSearchTool {
    pub(crate) fn pick_provider_from_env(exa_key: Option<String>, parallel_key: Option<String>, override_pref: Option<&str>) -> anyhow::Result<ProviderConfig> {
        let exa = exa_key.filter(|s| !s.trim().is_empty());
        let parallel = parallel_key.filter(|s| !s.trim().is_empty());

        match override_pref {
            Some("exa") => exa
                .map(|key| ProviderConfig {
                    provider: Provider::Exa,
                    url: EXA_HOST,
                    query_key: Some(key),
                    bearer: None,
                })
                .ok_or_else(|| anyhow::anyhow!("SMOOTH_WEBSEARCH_PROVIDER=exa but SMOOTH_EXA_API_KEY is unset")),
            Some("parallel") => parallel
                .map(|key| ProviderConfig {
                    provider: Provider::Parallel,
                    url: PARALLEL_URL,
                    query_key: None,
                    bearer: Some(key),
                })
                .ok_or_else(|| anyhow::anyhow!("SMOOTH_WEBSEARCH_PROVIDER=parallel but SMOOTH_PARALLEL_API_KEY is unset")),
            Some(other) => Err(anyhow::anyhow!("SMOOTH_WEBSEARCH_PROVIDER={other} unrecognised (expected 'exa' or 'parallel')")),
            None => {
                if let Some(key) = exa {
                    Ok(ProviderConfig {
                        provider: Provider::Exa,
                        url: EXA_HOST,
                        query_key: Some(key),
                        bearer: None,
                    })
                } else if let Some(key) = parallel {
                    Ok(ProviderConfig {
                        provider: Provider::Parallel,
                        url: PARALLEL_URL,
                        query_key: None,
                        bearer: Some(key),
                    })
                } else {
                    Err(anyhow::anyhow!(
                        "web_search has no provider configured — set SMOOTH_EXA_API_KEY or SMOOTH_PARALLEL_API_KEY"
                    ))
                }
            }
        }
    }

    fn pick_provider() -> anyhow::Result<ProviderConfig> {
        Self::pick_provider_from_env(
            std::env::var("SMOOTH_EXA_API_KEY").ok(),
            std::env::var("SMOOTH_PARALLEL_API_KEY").ok(),
            std::env::var("SMOOTH_WEBSEARCH_PROVIDER").ok().as_deref(),
        )
    }

    fn build_arguments(provider: Provider, args: &serde_json::Value) -> serde_json::Value {
        let query = args.get("query").and_then(|v| v.as_str()).unwrap_or_default();
        match provider {
            Provider::Exa => {
                let num_results = args.get("num_results").and_then(|v| v.as_u64()).unwrap_or(8);
                let search_type = args.get("type").and_then(|v| v.as_str()).unwrap_or("auto");
                let livecrawl = args.get("livecrawl").and_then(|v| v.as_str()).unwrap_or("fallback");
                let mut payload = json!({
                    "query": query,
                    "type": search_type,
                    "numResults": num_results,
                    "livecrawl": livecrawl,
                });
                if let Some(n) = args.get("context_max_characters").and_then(|v| v.as_u64()) {
                    payload["contextMaxCharacters"] = json!(n);
                }
                payload
            }
            Provider::Parallel => json!({
                "objective": query,
                "search_queries": [query],
            }),
        }
    }

    fn mcp_request(tool_name: &str, args: serde_json::Value) -> serde_json::Value {
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": tool_name,
                "arguments": args,
            }
        })
    }

    /// MCP responses are either a plain JSON `{ "result": { "content": [...] } }`
    /// or an SSE stream with `data: {...}` lines. OpenCode handles both — so do we.
    /// Returns the first content `text` field, or None if nothing parseable found.
    pub(crate) fn parse_response(body: &str) -> Option<String> {
        if let Some(text) = parse_mcp_payload(body.trim()) {
            return Some(text);
        }
        for line in body.split('\n') {
            if let Some(payload) = line.strip_prefix("data: ") {
                if let Some(text) = parse_mcp_payload(payload.trim()) {
                    return Some(text);
                }
            }
        }
        None
    }
}

fn parse_mcp_payload(payload: &str) -> Option<String> {
    if !payload.starts_with('{') {
        return None;
    }
    let value: serde_json::Value = serde_json::from_str(payload).ok()?;
    let content = value.get("result")?.get("content")?.as_array()?;
    for item in content {
        if let Some(text) = item.get("text").and_then(|t| t.as_str()) {
            if !text.is_empty() {
                return Some(text.to_string());
            }
        }
    }
    None
}

#[async_trait]
impl Tool for WebSearchTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "web_search".to_string(),
            description: "Search the web via a hosted LLM-tuned provider (Exa, with Parallel fallback). Returns extracted, LLM-ready text — no separate fetch step needed for the snippets. Use for: time-sensitive questions, fact-checking, finding documentation, identifying a title from a description. Knobs match OpenCode's surface so smooth-vs-opencode benches stay comparable.".to_string(),
            parameters: json!({
                "type": "object",
                "required": ["query"],
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "The search query. Be specific — Exa rewards descriptive queries."
                    },
                    "num_results": {
                        "type": "integer",
                        "description": "How many results to return (default: 8). Exa-only.",
                        "minimum": 1,
                        "maximum": 25
                    },
                    "type": {
                        "type": "string",
                        "enum": ["auto", "fast", "deep"],
                        "description": "Search depth: 'fast' for quick lookups, 'deep' for comprehensive research, 'auto' (default) for balanced. Exa-only."
                    },
                    "livecrawl": {
                        "type": "string",
                        "enum": ["fallback", "preferred"],
                        "description": "Cache vs live-crawl preference. 'fallback' (default) prefers cached content, 'preferred' forces live crawling. Exa-only."
                    },
                    "context_max_characters": {
                        "type": "integer",
                        "description": "Cap on the extracted-context string length (Exa-only). Useful when the agent is near its context limit.",
                        "minimum": 1000
                    }
                }
            }),
        }
    }

    async fn execute(&self, arguments: serde_json::Value) -> anyhow::Result<String> {
        let query = arguments
            .get("query")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing 'query'"))?;
        if query.trim().is_empty() {
            return Err(anyhow::anyhow!("'query' must be a non-empty string"));
        }

        let cfg = Self::pick_provider()?;
        let tool_name = match cfg.provider {
            Provider::Exa => "web_search_exa",
            Provider::Parallel => "web_search",
        };
        let args = Self::build_arguments(cfg.provider, &arguments);
        let body = Self::mcp_request(tool_name, args);

        let mut request = self
            .client
            .post(cfg.url)
            .header("Accept", "application/json, text/event-stream")
            .header("Content-Type", "application/json")
            .timeout(std::time::Duration::from_secs(30))
            .json(&body);
        if let Some(key) = cfg.query_key.as_ref() {
            request = request.query(&[("exaApiKey", key.as_str())]);
        }
        if let Some(token) = cfg.bearer.as_ref() {
            request = request.header("Authorization", format!("Bearer {token}"));
        }

        let resp = request
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("web_search ({}) request failed: {e}", cfg.provider.label()))?;
        let status = resp.status();
        let payload = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            // Truncate provider error so an HTML error page can't blow the
            // transcript. Most MCP errors come back as JSON already.
            let snippet: String = payload.chars().take(512).collect();
            return Err(anyhow::anyhow!("web_search ({}) returned {status}: {snippet}", cfg.provider.label()));
        }

        match Self::parse_response(&payload) {
            Some(text) => Ok(format!("[provider: {}]\n{text}", cfg.provider.label())),
            None => Err(anyhow::anyhow!(
                "web_search ({}) returned an empty / unparseable MCP envelope",
                cfg.provider.label()
            )),
        }
    }

    fn is_read_only(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_mcp(text: &str) -> String {
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {
                "content": [
                    { "type": "text", "text": text }
                ]
            }
        })
        .to_string()
    }

    #[test]
    fn parses_plain_mcp_envelope() {
        let body = fake_mcp("search result body");
        assert_eq!(WebSearchTool::parse_response(&body), Some("search result body".to_string()));
    }

    #[test]
    fn parses_sse_envelope() {
        let inner = fake_mcp("sse search result");
        let body = format!("event: message\ndata: {inner}\n\n");
        assert_eq!(WebSearchTool::parse_response(&body), Some("sse search result".to_string()));
    }

    #[test]
    fn parses_first_nonempty_text_among_content() {
        let body = json!({
            "result": {
                "content": [
                    { "type": "text", "text": "" },
                    { "type": "text", "text": "second wins" },
                    { "type": "text", "text": "third unused" }
                ]
            }
        })
        .to_string();
        assert_eq!(WebSearchTool::parse_response(&body), Some("second wins".to_string()));
    }

    #[test]
    fn unparseable_returns_none() {
        assert_eq!(WebSearchTool::parse_response("not json"), None);
        assert_eq!(WebSearchTool::parse_response(""), None);
        assert_eq!(WebSearchTool::parse_response("{\"result\": {}}"), None);
    }

    #[test]
    fn builds_exa_arguments_with_defaults() {
        let args = WebSearchTool::build_arguments(Provider::Exa, &json!({"query": "hello"}));
        assert_eq!(args["query"], json!("hello"));
        assert_eq!(args["type"], json!("auto"));
        assert_eq!(args["numResults"], json!(8));
        assert_eq!(args["livecrawl"], json!("fallback"));
        assert!(args.get("contextMaxCharacters").is_none());
    }

    #[test]
    fn builds_exa_arguments_with_overrides() {
        let args = WebSearchTool::build_arguments(
            Provider::Exa,
            &json!({
                "query": "deep research",
                "type": "deep",
                "num_results": 12,
                "livecrawl": "preferred",
                "context_max_characters": 4000
            }),
        );
        assert_eq!(args["type"], json!("deep"));
        assert_eq!(args["numResults"], json!(12));
        assert_eq!(args["livecrawl"], json!("preferred"));
        assert_eq!(args["contextMaxCharacters"], json!(4000));
    }

    #[test]
    fn builds_parallel_arguments() {
        let args = WebSearchTool::build_arguments(Provider::Parallel, &json!({"query": "find me"}));
        assert_eq!(args["objective"], json!("find me"));
        assert_eq!(args["search_queries"], json!(["find me"]));
    }

    #[test]
    fn mcp_request_shape_matches_opencode() {
        let req = WebSearchTool::mcp_request("web_search_exa", json!({"query": "x"}));
        assert_eq!(req["jsonrpc"], json!("2.0"));
        assert_eq!(req["id"], json!(1));
        assert_eq!(req["method"], json!("tools/call"));
        assert_eq!(req["params"]["name"], json!("web_search_exa"));
        assert_eq!(req["params"]["arguments"]["query"], json!("x"));
    }

    #[test]
    fn picks_exa_when_only_exa_key_set() {
        let cfg = WebSearchTool::pick_provider_from_env(Some("EXA-KEY".into()), None, None).unwrap();
        assert_eq!(cfg.provider, Provider::Exa);
        assert_eq!(cfg.query_key.as_deref(), Some("EXA-KEY"));
        assert!(cfg.bearer.is_none());
        assert_eq!(cfg.url, EXA_HOST);
    }

    #[test]
    fn picks_parallel_when_only_parallel_key_set() {
        let cfg = WebSearchTool::pick_provider_from_env(None, Some("PAR-KEY".into()), None).unwrap();
        assert_eq!(cfg.provider, Provider::Parallel);
        assert_eq!(cfg.bearer.as_deref(), Some("PAR-KEY"));
        assert!(cfg.query_key.is_none());
        assert_eq!(cfg.url, PARALLEL_URL);
    }

    #[test]
    fn prefers_exa_when_both_set_and_no_override() {
        let cfg = WebSearchTool::pick_provider_from_env(Some("E".into()), Some("P".into()), None).unwrap();
        assert_eq!(cfg.provider, Provider::Exa);
    }

    #[test]
    fn override_forces_parallel_even_when_exa_present() {
        let cfg = WebSearchTool::pick_provider_from_env(Some("E".into()), Some("P".into()), Some("parallel")).unwrap();
        assert_eq!(cfg.provider, Provider::Parallel);
    }

    #[test]
    fn override_to_exa_without_key_errors() {
        let err = WebSearchTool::pick_provider_from_env(None, Some("P".into()), Some("exa")).unwrap_err();
        assert!(err.to_string().contains("SMOOTH_EXA_API_KEY is unset"), "got: {err}");
    }

    #[test]
    fn unrecognised_override_errors() {
        let err = WebSearchTool::pick_provider_from_env(Some("E".into()), None, Some("brave")).unwrap_err();
        assert!(err.to_string().contains("unrecognised"), "got: {err}");
    }

    #[test]
    fn no_keys_errors_actionably() {
        let err = WebSearchTool::pick_provider_from_env(None, None, None).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("SMOOTH_EXA_API_KEY"), "got: {msg}");
        assert!(msg.contains("SMOOTH_PARALLEL_API_KEY"), "got: {msg}");
    }

    #[test]
    fn empty_key_strings_are_ignored() {
        let err = WebSearchTool::pick_provider_from_env(Some("   ".into()), Some("".into()), None).unwrap_err();
        assert!(err.to_string().contains("no provider configured"), "got: {err}");
    }

    #[tokio::test]
    async fn end_to_end_against_mock_mcp_server() {
        use axum::{routing::post, Json, Router};
        use tokio::net::TcpListener;

        let app = Router::new().route(
            "/mcp",
            post(|Json(body): Json<serde_json::Value>| async move {
                let name = body["params"]["name"].as_str().unwrap_or("").to_string();
                let query = body["params"]["arguments"]["query"].as_str().unwrap_or("").to_string();
                Json(json!({
                    "result": {
                        "content": [
                            { "type": "text", "text": format!("{name} on {query}") }
                        ]
                    }
                }))
            }),
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        // We can't easily monkey-patch the constants, so the e2e check goes
        // through `build_arguments` + `mcp_request` + parse_response on a
        // round-trip we drive ourselves. This exercises the wire shape
        // exactly as the live request would.
        let client = reqwest::Client::new();
        let args = WebSearchTool::build_arguments(Provider::Exa, &json!({"query": "alpha"}));
        let body = WebSearchTool::mcp_request("web_search_exa", args);
        let resp = client.post(format!("http://{addr}/mcp")).json(&body).send().await.unwrap();
        assert!(resp.status().is_success());
        let text = resp.text().await.unwrap();
        assert_eq!(WebSearchTool::parse_response(&text), Some("web_search_exa on alpha".to_string()));
    }
}

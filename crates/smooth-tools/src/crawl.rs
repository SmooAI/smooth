//! `crawl` — a first-class named tool over `th crawl scrape`.
//!
//! Fetch a specific web page and return it as clean markdown (as opposed to
//! [`web_search`](crate::WebSearchTool), which finds pages). A dedicated typed
//! tool so the model reliably reaches for "read this URL" when given a link.
//! Shells `th crawl scrape` — argv only, no shell.

use std::path::PathBuf;

use async_trait::async_trait;
use serde_json::{json, Value};
use smooth_operator::{Tool, ToolSchema};

/// `crawl` — fetch a URL and return it as clean markdown.
pub struct CrawlTool {
    /// Working directory for the `th` invocation.
    pub workspace: PathBuf,
}

#[async_trait]
impl Tool for CrawlTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "crawl".into(),
            description: "Fetch a specific web page by URL and return its main content as clean markdown. Use this when you have a link (from web_search or the user) and need to actually READ the page. For finding pages, use web_search instead. Fetches over the network; that is intended."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "url": {
                        "type": "string",
                        "description": "The full URL to fetch, e.g. \"https://docs.rs/tokio\"."
                    }
                },
                "required": ["url"]
            }),
        }
    }

    fn is_concurrent_safe(&self) -> bool {
        true
    }

    async fn execute(&self, arguments: Value) -> anyhow::Result<String> {
        let url = require_url(&arguments)?;
        crate::th::run_th(&["crawl".to_owned(), "scrape".to_owned(), url], &self.workspace).await
    }
}

fn require_url(arguments: &Value) -> anyhow::Result<String> {
    let url = arguments
        .get("url")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|u| !u.is_empty())
        .ok_or_else(|| anyhow::anyhow!("missing required string parameter `url`"))?;
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        anyhow::bail!("`url` must be an http(s) URL, got: {url}");
    }
    Ok(url.to_owned())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, reason = "unwrap/expect are the idiom for test assertions")]
mod tests {
    use super::*;

    fn tool() -> CrawlTool {
        CrawlTool {
            workspace: std::env::temp_dir(),
        }
    }

    #[test]
    fn schema_names_crawl_and_requires_url() {
        let s = tool().schema();
        assert_eq!(s.name, "crawl");
        assert_eq!(s.parameters["required"][0], "url");
        assert!(tool().is_concurrent_safe());
    }

    #[test]
    fn require_url_accepts_http_and_rejects_others() {
        assert_eq!(require_url(&json!({"url": "https://x.com"})).unwrap(), "https://x.com");
        assert!(require_url(&json!({"url": "ftp://x"})).is_err(), "non-http rejected");
        assert!(require_url(&json!({"url": "not a url"})).is_err());
        assert!(require_url(&json!({})).is_err());
    }

    #[tokio::test]
    async fn execute_rejects_missing_url() {
        assert!(tool().execute(json!({})).await.is_err());
    }
}

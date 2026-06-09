//! `host_tool` operator tool.
//!
//! Lets the teammate (running inside a Microsandbox VM) invoke a small
//! whitelist of host CLIs (`gh`, `git`, `kubectl`, `jq`, `curl`) without
//! shipping the user's host credentials into the sandbox. Calls Big
//! Smooth's `/api/host/exec` over the already-allowed
//! `host.containers.internal:4400` route (set as `SMOOTH_NARC_URL`).
//!
//! See `crates/smooth-bigsmooth/src/host_tools.rs` for the host side —
//! that file owns the allowlist and the per-process bearer token. The
//! token is threaded into the sandbox via `SMOOTH_HOST_TOKEN` env at
//! dispatch time.

use async_trait::async_trait;
use serde_json::json;
use smooth_operator::tool::{Tool, ToolSchema};

pub struct HostToolTool;

#[async_trait]
impl Tool for HostToolTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "host_tool".to_string(),
            description: "Run a whitelisted host CLI (defaults: gh, git, kubectl, jq, curl) on Big Smooth's host machine. Inherits the user's host-side auth so private repo listings, kube cluster access, etc. work without secrets in the sandbox. Returns stdout, stderr, exit_code. 30 s timeout, 8 KiB output cap. Use for read-only lookups; mutating commands are still subject to the host's allowlist.".to_string(),
            parameters: json!({
                "type": "object",
                "required": ["tool"],
                "properties": {
                    "tool": {
                        "type": "string",
                        "description": "CLI to invoke (must be in the host's allowlist — `gh`, `git`, `kubectl`, `jq`, `curl` by default)."
                    },
                    "args": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Argument vector. Pass each flag/value as a separate string element. Example: tool=`gh` args=[`repo`,`list`,`--limit`,`50`]."
                    }
                }
            }),
        }
    }

    async fn execute(&self, arguments: serde_json::Value) -> anyhow::Result<String> {
        let tool = arguments["tool"].as_str().ok_or_else(|| anyhow::anyhow!("missing 'tool'"))?;
        let args: Vec<String> = arguments
            .get("args")
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
            .unwrap_or_default();

        let narc_url =
            std::env::var("SMOOTH_NARC_URL").map_err(|_| anyhow::anyhow!("SMOOTH_NARC_URL not set — host_tool only works under sandbox dispatch"))?;
        let token =
            std::env::var("SMOOTH_HOST_TOKEN").map_err(|_| anyhow::anyhow!("SMOOTH_HOST_TOKEN not set — Big Smooth didn't pass through the host bearer"))?;

        let url = format!("{}/api/host/exec", narc_url.trim_end_matches('/'));
        let body = serde_json::json!({ "tool": tool, "args": args });

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(35))
            .build()
            .map_err(|e| anyhow::anyhow!("building http client: {e}"))?;
        let resp = client
            .post(&url)
            .header("Authorization", format!("Bearer {token}"))
            .json(&body)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("calling host_exec: {e}"))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let txt = resp.text().await.unwrap_or_default();
            return Err(anyhow::anyhow!("host_exec returned {status}: {txt}"));
        }
        let payload: serde_json::Value = resp.json().await.map_err(|e| anyhow::anyhow!("parsing host_exec response: {e}"))?;
        // Render as a single combined block so the agent's transcript
        // stays readable. Strip empty fields.
        let stdout = payload.get("stdout").and_then(|v| v.as_str()).unwrap_or("");
        let stderr = payload.get("stderr").and_then(|v| v.as_str()).unwrap_or("");
        let exit = payload.get("exit_code").and_then(|v| v.as_i64()).unwrap_or(-1);
        let truncated = payload.get("truncated").and_then(|v| v.as_bool()).unwrap_or(false);
        let mut out = String::new();
        if !stdout.is_empty() {
            out.push_str(stdout);
        }
        if !stderr.is_empty() {
            if !out.is_empty() && !out.ends_with('\n') {
                out.push('\n');
            }
            out.push_str("[stderr] ");
            out.push_str(stderr);
        }
        if exit != 0 {
            out.push_str(&format!("\n[exit {exit}]"));
        }
        if truncated {
            out.push_str("\n[truncated]");
        }
        if out.is_empty() {
            out = "(no output)".into();
        }
        Ok(out)
    }

    fn is_read_only(&self) -> bool {
        // Host CLIs vary; `gh repo list` is read-only but `gh repo create`
        // isn't. Conservative.
        false
    }
}

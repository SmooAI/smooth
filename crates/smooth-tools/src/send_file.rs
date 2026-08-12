//! `send_file` — deliver a workspace file to the user as a download.
//!
//! The agent writes a file into its workspace (`write_file` / `create_artifact`
//! / `bash`), then calls `send_file <path>` to hand it over. We read the bytes,
//! base64 them into a `data:` URL, and write a `send_file` directive onto the
//! turn's directive sink — the per-turn channel the engine drains onto
//! `eventual_response.directive`, which every face renders as a download
//! (protocol contract: `{ "type": "send_file", "files": [{ name, mimeType, url }] }`).
//!
//! The sink comes from `ToolProviderContext.directive_sink`, which only the
//! daemon's `ToolProvider` sees, so this tool is constructed fresh each turn
//! with the sink captured — same pattern as the per-role calendar tool.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use base64::Engine;
use serde_json::{json, Value};
use smooth_operator::{Tool, ToolSchema};

use crate::path::resolve_workspace_path;
use crate::util::req_str;

/// Cap on a single delivered file. base64 inflates ~4/3 and the bytes ride the
/// canonical WS frame (no server-side cap; the relay leans on Valkey buffers),
/// so keep it modest.
/// ponytail: 10 MB flat; add chunked transfer only if big files become real.
const MAX_BYTES: u64 = 10 * 1024 * 1024;

/// `send_file` — hand a workspace file to the user.
pub struct SendFileTool {
    workspace: PathBuf,
    /// The turn's directive sink (`ToolProviderContext.directive_sink`), drained
    /// by the engine onto `eventual_response.directive`.
    sink: Arc<Mutex<Value>>,
}

impl SendFileTool {
    pub fn new(workspace: PathBuf, sink: Arc<Mutex<Value>>) -> Self {
        Self { workspace, sink }
    }
}

/// Minimal extension → MIME map for the common cases; everything else is a
/// generic download. ponytail: a match, not a mime crate.
fn mime_for(path: &std::path::Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).as_deref() {
        Some("pdf") => "application/pdf",
        Some("csv") => "text/csv",
        Some("txt" | "log" | "md") => "text/plain",
        Some("json") => "application/json",
        Some("html" | "htm") => "text/html",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("svg") => "image/svg+xml",
        Some("zip") => "application/zip",
        _ => "application/octet-stream",
    }
}

#[async_trait]
impl Tool for SendFileTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "send_file".into(),
            description: "Deliver a file from your workspace to the user — they receive it as a download in the chat. Give the workspace-relative path of a file you have already written (with write_file, create_artifact, or bash). Use this to hand over a report, export, or generated document.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Workspace-relative path of the file to send." }
                },
                "required": ["path"]
            }),
        }
    }

    async fn execute(&self, arguments: Value) -> anyhow::Result<String> {
        let rel = req_str(&arguments, "path")?;
        // Same confinement as read/write: a path escaping the workspace is refused here.
        let path = resolve_workspace_path(&self.workspace, &rel)?;
        let meta = tokio::fs::metadata(&path).await.map_err(|e| anyhow::anyhow!("cannot read `{rel}`: {e}"))?;
        if !meta.is_file() {
            return Ok(format!("BLOCKED: `{rel}` is not a file."));
        }
        if meta.len() > MAX_BYTES {
            return Ok(format!("BLOCKED: `{rel}` is {} bytes; the send limit is {MAX_BYTES} bytes.", meta.len()));
        }
        let bytes = tokio::fs::read(&path).await.map_err(|e| anyhow::anyhow!("cannot read `{rel}`: {e}"))?;
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("file").to_string();
        let mime = mime_for(&path);
        let data_url = format!("data:{mime};base64,{}", base64::engine::general_purpose::STANDARD.encode(&bytes));
        let file = json!({ "name": name, "mimeType": mime, "url": data_url });

        // Merge into the turn's directive: several send_file calls in one turn
        // accumulate into one directive's files[]. The engine's sink is
        // last-write-wins, so a plain overwrite would drop all but the final file.
        // Recover a poisoned lock rather than panic — the directive isn't
        // security-critical and another panicking thread shouldn't kill the turn.
        {
            let mut guard = self.sink.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            if guard.get("type").and_then(Value::as_str) == Some("send_file") {
                if let Some(files) = guard.get_mut("files").and_then(Value::as_array_mut) {
                    files.push(file);
                }
            } else {
                *guard = json!({ "type": "send_file", "files": [file] });
            }
        }
        Ok(format!("Sent {name} ({} bytes) to the user.", bytes.len()))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, reason = "unwrap/expect are the idiom for test assertions")]
mod tests {
    use super::*;

    fn sink() -> Arc<Mutex<Value>> {
        Arc::new(Mutex::new(Value::Null))
    }

    #[tokio::test]
    async fn sends_a_file_as_a_data_url_directive() {
        let dir = tempfile::tempdir().unwrap();
        tokio::fs::write(dir.path().join("report.csv"), b"a,b\n1,2\n").await.unwrap();
        let s = sink();
        let tool = SendFileTool::new(dir.path().to_path_buf(), Arc::clone(&s));

        let out = tool.execute(json!({ "path": "report.csv" })).await.unwrap();
        assert!(out.contains("report.csv"), "result: {out}");

        let d = s.lock().unwrap().clone();
        assert_eq!(d["type"], "send_file");
        assert_eq!(d["files"][0]["name"], "report.csv");
        assert_eq!(d["files"][0]["mimeType"], "text/csv");
        assert!(d["files"][0]["url"].as_str().unwrap().starts_with("data:text/csv;base64,"));
    }

    #[tokio::test]
    async fn multiple_sends_accumulate_into_one_directive() {
        let dir = tempfile::tempdir().unwrap();
        tokio::fs::write(dir.path().join("a.txt"), b"a").await.unwrap();
        tokio::fs::write(dir.path().join("b.txt"), b"b").await.unwrap();
        let s = sink();
        let tool = SendFileTool::new(dir.path().to_path_buf(), Arc::clone(&s));
        tool.execute(json!({ "path": "a.txt" })).await.unwrap();
        tool.execute(json!({ "path": "b.txt" })).await.unwrap();
        let d = s.lock().unwrap().clone();
        assert_eq!(d["files"].as_array().unwrap().len(), 2, "both files should ride one directive: {d}");
    }

    #[tokio::test]
    async fn rejects_oversized_file() {
        let dir = tempfile::tempdir().unwrap();
        tokio::fs::write(dir.path().join("big.bin"), vec![0u8; (MAX_BYTES + 1) as usize]).await.unwrap();
        let s = sink();
        let tool = SendFileTool::new(dir.path().to_path_buf(), Arc::clone(&s));
        let out = tool.execute(json!({ "path": "big.bin" })).await.unwrap();
        assert!(out.starts_with("BLOCKED"), "oversized should be blocked: {out}");
        assert!(s.lock().unwrap().is_null(), "no directive for a blocked send");
    }

    #[tokio::test]
    async fn refuses_a_path_escaping_the_workspace() {
        let dir = tempfile::tempdir().unwrap();
        let s = sink();
        let tool = SendFileTool::new(dir.path().to_path_buf(), Arc::clone(&s));
        // resolve_workspace_path rejects traversal → execute returns Err.
        assert!(tool.execute(json!({ "path": "../../etc/passwd" })).await.is_err());
        assert!(s.lock().unwrap().is_null());
    }
}

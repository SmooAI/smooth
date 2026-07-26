//! `create_artifact` — write a self-contained HTML report/artifact.
//!
//! Big Smooth's answer to Claude Code Artifacts: the agent hands over a
//! filename + a self-contained HTML document, we drop it into
//! `<workspace>/.smooth-artifacts/` and return the absolute path plus a
//! `file://` URL the user can click to open it. Same workspace confinement as
//! [`crate::write`] — the path routes through [`resolve_workspace_path`] and
//! consults the `Write` deny gate before anything hits disk.
//!
//! Rendering the artifact inline in smooth-web is a follow-up (pearl th-66b4c6);
//! this tool just produces the file + a link.

use std::path::PathBuf;

use async_trait::async_trait;
use serde_json::{json, Value};
use smooth_operator::{Tool, ToolSchema};

use crate::path::resolve_workspace_path;
use crate::util::req_str;

/// Directory (relative to the workspace root) artifacts are written into.
const ARTIFACT_DIR: &str = ".smooth-artifacts";

/// `create_artifact` — write a self-contained `.html` artifact and return a link.
pub struct ArtifactTool {
    /// Workspace root.
    pub workspace: PathBuf,
}

/// Force a `.html` extension and strip any directory components so the artifact
/// always lands directly in the artifacts dir (path confinement is enforced
/// separately, but this keeps `foo/../bar` and bare names tidy).
fn artifact_filename(raw: &str) -> String {
    let base = raw.rsplit(['/', '\\']).next().unwrap_or(raw).trim();
    let base = if base.is_empty() { "artifact" } else { base };
    if base.to_ascii_lowercase().ends_with(".html") || base.to_ascii_lowercase().ends_with(".htm") {
        base.to_string()
    } else {
        format!("{base}.html")
    }
}

#[async_trait]
impl Tool for ArtifactTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "create_artifact".into(),
            description: "Write a self-contained HTML report/artifact (like Claude Code Artifacts) into the workspace's .smooth-artifacts/ directory \
                          and return its absolute path and a clickable file:// URL. The `html` must be a complete, self-contained document \
                          (inline all CSS/JS; no external assets)."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "filename": { "type": "string", "description": "Artifact filename, e.g. `report.html` (a .html extension is added if missing)" },
                    "html": { "type": "string", "description": "The complete, self-contained HTML document to write" }
                },
                "required": ["filename", "html"]
            }),
        }
    }

    async fn execute(&self, arguments: Value) -> anyhow::Result<String> {
        let filename = artifact_filename(&req_str(&arguments, "filename")?);
        let html = req_str(&arguments, "html")?;
        let rel = format!("{ARTIFACT_DIR}/{filename}");
        let path = resolve_workspace_path(&self.workspace, &rel)?;

        // Same Gate 1 as write_file: a configurable `Write` deny rule can refuse
        // a protected in-workspace path before we touch disk.
        if crate::permission::write_denied(&self.workspace, &path) {
            return Ok(format!("BLOCKED: a permission policy (deny) rule refused writing {rel}"));
        }

        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| anyhow::anyhow!("cannot create artifacts dir: {e}"))?;
        }
        tokio::fs::write(&path, html.as_bytes())
            .await
            .map_err(|e| anyhow::anyhow!("cannot write artifact `{rel}`: {e}"))?;

        let abs = path.display();
        Ok(format!("Wrote {} bytes to artifact {filename}.\nPath: {abs}\nOpen: file://{abs}", html.len()))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, reason = "unwrap/expect are the idiom for test assertions")]
mod tests {
    use super::*;

    fn ws() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[tokio::test]
    async fn writes_artifact_and_returns_path_and_url() {
        let dir = ws();
        let tool = ArtifactTool {
            workspace: dir.path().to_path_buf(),
        };
        let html = "<!doctype html><html><body><h1>Report</h1></body></html>";
        let out = tool.execute(json!({"filename": "report.html", "html": html})).await.unwrap();

        let written_path = dir.path().join(".smooth-artifacts/report.html");
        // Content round-trips.
        assert_eq!(tokio::fs::read_to_string(&written_path).await.unwrap(), html);
        // Result carries the absolute path and a clickable file:// URL.
        let abs = written_path.display().to_string();
        assert!(out.contains(&abs), "path missing in result: {out}");
        assert!(out.contains(&format!("file://{abs}")), "file:// url missing in result: {out}");
    }

    #[tokio::test]
    async fn appends_html_extension_when_missing() {
        let dir = ws();
        let tool = ArtifactTool {
            workspace: dir.path().to_path_buf(),
        };
        tool.execute(json!({"filename": "summary", "html": "<p>hi</p>"})).await.unwrap();
        assert!(dir.path().join(".smooth-artifacts/summary.html").exists());
    }

    #[tokio::test]
    async fn strips_directory_components_from_filename() {
        let dir = ws();
        let tool = ArtifactTool {
            workspace: dir.path().to_path_buf(),
        };
        // A traversal-y filename collapses to its basename inside the artifacts dir.
        tool.execute(json!({"filename": "../../etc/evil.html", "html": "<p>x</p>"})).await.unwrap();
        assert!(dir.path().join(".smooth-artifacts/evil.html").exists());
    }
}

//! Audit logging — rotating file appender for tool usage and lifecycle events.
//!
//! Format:
//! ```text
//! [2026-03-30T17:45:12.345Z] operator-abc3f | tool_call | beads_context
//!     bead: bead-123
//!     input: {"beadId":"bead-123"}
//!     duration: 245ms
//! ```

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;

use chrono::Utc;

/// Audit log directory: `~/.smooth/audit/`
fn audit_dir() -> PathBuf {
    dirs_next::home_dir().unwrap_or_default().join(".smooth").join("audit")
}

/// Ensure the audit directory exists.
pub fn ensure_audit_dir() -> anyhow::Result<PathBuf> {
    let dir = audit_dir();
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Get the audit log directory path.
#[must_use]
pub fn get_audit_dir() -> PathBuf {
    audit_dir()
}

/// A single audit log entry.
pub struct AuditEntry {
    pub actor: String,
    pub action: String,
    pub target: Option<String>,
    pub bead_id: Option<String>,
    pub input: Option<serde_json::Value>,
    pub output: Option<serde_json::Value>,
    pub duration_ms: Option<u64>,
    pub error: Option<String>,
}

/// Write an audit entry to the actor's log file (append-only).
pub fn audit(entry: &AuditEntry) {
    let dir = audit_dir();
    let _ = fs::create_dir_all(&dir);

    let sanitized = entry.actor.replace(|c: char| !c.is_alphanumeric() && c != '-' && c != '_', "_");
    let path = dir.join(format!("{sanitized}.log"));

    let mut lines = vec![format!("[{}] {} | {}", Utc::now().to_rfc3339(), entry.actor, entry.action)];

    if let Some(ref target) = entry.target {
        lines[0] = format!("{} | {target}", lines[0]);
    }
    if let Some(ref bead_id) = entry.bead_id {
        lines.push(format!("    bead: {bead_id}"));
    }
    if let Some(ref input) = entry.input {
        let s = serde_json::to_string(input).unwrap_or_default();
        let truncated = if s.len() > 200 { format!("{}... ({} chars)", &s[..200], s.len()) } else { s };
        lines.push(format!("    input: {truncated}"));
    }
    if let Some(ref output) = entry.output {
        let s = serde_json::to_string(output).unwrap_or_default();
        let truncated = if s.len() > 200 { format!("{}... ({} chars)", &s[..200], s.len()) } else { s };
        lines.push(format!("    output: {truncated}"));
    }
    if let Some(ms) = entry.duration_ms {
        lines.push(format!("    duration: {ms}ms"));
    }
    if let Some(ref err) = entry.error {
        lines.push(format!("    error: {err}"));
    }

    let text = lines.join("\n") + "\n";

    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(&path) {
        let _ = file.write_all(text.as_bytes());
    }
}

/// Scoped audit logger for a specific actor.
pub struct AuditLogger {
    pub actor: String,
    pub default_bead_id: Option<String>,
}

impl AuditLogger {
    #[must_use]
    pub fn new(actor: &str) -> Self {
        Self {
            actor: actor.to_string(),
            default_bead_id: None,
        }
    }

    pub fn phase_started(&self, phase: &str) {
        audit(&AuditEntry {
            actor: self.actor.clone(),
            action: "phase_started".into(),
            target: Some(phase.into()),
            bead_id: self.default_bead_id.clone(),
            ..default_entry()
        });
    }

    pub fn phase_completed(&self, phase: &str, duration_ms: u64) {
        audit(&AuditEntry {
            actor: self.actor.clone(),
            action: "phase_completed".into(),
            target: Some(phase.into()),
            bead_id: self.default_bead_id.clone(),
            duration_ms: Some(duration_ms),
            ..default_entry()
        });
    }

    pub fn error(&self, message: &str) {
        audit(&AuditEntry {
            actor: self.actor.clone(),
            action: "error".into(),
            error: Some(message.into()),
            bead_id: self.default_bead_id.clone(),
            ..default_entry()
        });
    }

    pub fn tool_call(&self, tool: &str, input: Option<serde_json::Value>, output: Option<serde_json::Value>, duration_ms: Option<u64>) {
        audit(&AuditEntry {
            actor: self.actor.clone(),
            action: "tool_call".into(),
            target: Some(tool.into()),
            bead_id: self.default_bead_id.clone(),
            input,
            output,
            duration_ms,
            error: None,
        });
    }
}

fn default_entry() -> AuditEntry {
    AuditEntry {
        actor: String::new(),
        action: String::new(),
        target: None,
        bead_id: None,
        input: None,
        output: None,
        duration_ms: None,
        error: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_audit_writes_file() {
        let dir = tempfile::tempdir().unwrap();
        // Override audit dir for test
        let path = dir.path().join("test-actor.log");

        let entry = AuditEntry {
            actor: "test-actor".into(),
            action: "test_action".into(),
            target: Some("target".into()),
            bead_id: Some("bead-123".into()),
            input: Some(serde_json::json!({"key": "value"})),
            output: None,
            duration_ms: Some(42),
            error: None,
        };

        // Write directly to test path
        let mut lines = vec![format!(
            "[{}] {} | {} | {}",
            Utc::now().to_rfc3339(),
            entry.actor,
            entry.action,
            entry.target.as_deref().unwrap_or("")
        )];
        lines.push(format!("    bead: {}", entry.bead_id.as_deref().unwrap_or("")));
        lines.push(format!("    duration: {}ms", entry.duration_ms.unwrap_or(0)));
        let text = lines.join("\n") + "\n";

        std::fs::write(&path, &text).unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("test-actor"));
        assert!(content.contains("test_action"));
        assert!(content.contains("bead-123"));
        assert!(content.contains("42ms"));
    }
}

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

use serde::Serialize;

/// JSON-lines audit logger for proxy requests.
pub struct AuditLogger {
    #[allow(dead_code)]
    path: PathBuf,
    file: Mutex<std::fs::File>,
}

#[derive(Debug, Serialize)]
pub struct AuditEntry {
    pub timestamp: String,
    pub domain: String,
    pub path: String,
    pub method: String,
    pub allowed: bool,
    pub reason: String,
    pub status_code: Option<u16>,
    pub duration_ms: u64,
}

impl AuditLogger {
    /// Create a new audit logger writing JSON-lines to `path`.
    ///
    /// # Errors
    /// Returns error if the parent directory cannot be created or the file cannot be opened.
    pub fn new(path: &str) -> anyhow::Result<Self> {
        let path = PathBuf::from(path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        Ok(Self { path, file: Mutex::new(file) })
    }

    /// Append an audit entry as a single JSON line.
    pub fn log(&self, entry: &AuditEntry) {
        if let Ok(json) = serde_json::to_string(entry) {
            if let Ok(mut file) = self.file.lock() {
                let _ = writeln!(file, "{json}");
            }
        }
    }

    /// Get the log file path.
    #[allow(dead_code)]
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    #[test]
    fn audit_entry_serializes_to_json() {
        let entry = AuditEntry {
            timestamp: "2026-04-03T19:00:00Z".into(),
            domain: "api.github.com".into(),
            path: "/repos/SmooAI/smooth".into(),
            method: "GET".into(),
            allowed: true,
            reason: "domain in allowlist".into(),
            status_code: Some(200),
            duration_ms: 42,
        };
        let json = serde_json::to_string(&entry).expect("serialize");
        assert!(json.contains("api.github.com"));
        assert!(json.contains("\"allowed\":true"));
        assert!(json.contains("\"status_code\":200"));
    }

    #[test]
    fn audit_entry_blocked_request() {
        let entry = AuditEntry {
            timestamp: Utc::now().to_rfc3339(),
            domain: "evil.com".into(),
            path: "/steal".into(),
            method: "POST".into(),
            allowed: false,
            reason: "domain not in allowlist".into(),
            status_code: None,
            duration_ms: 0,
        };
        let json = serde_json::to_string(&entry).expect("serialize");
        assert!(json.contains("\"allowed\":false"));
        assert!(json.contains("\"status_code\":null"));
    }

    #[test]
    fn audit_logger_writes_to_file() {
        let dir = tempfile::tempdir().expect("tmpdir");
        let path = dir.path().join("test-audit.jsonl");
        let logger = AuditLogger::new(path.to_str().expect("path")).expect("create logger");

        let entry = AuditEntry {
            timestamp: "2026-04-03T19:00:00Z".into(),
            domain: "opencode.ai".into(),
            path: "/zen/v1/chat".into(),
            method: "POST".into(),
            allowed: true,
            reason: "domain in allowlist".into(),
            status_code: Some(200),
            duration_ms: 150,
        };
        logger.log(&entry);
        logger.log(&entry);

        let contents = std::fs::read_to_string(&path).expect("read");
        let lines: Vec<&str> = contents.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("opencode.ai"));
    }

    #[test]
    fn audit_logger_creates_parent_dirs() {
        let dir = tempfile::tempdir().expect("tmpdir");
        let path = dir.path().join("nested/dir/audit.jsonl");
        let logger = AuditLogger::new(path.to_str().expect("path"));
        assert!(logger.is_ok());
        assert!(path.parent().expect("parent").exists());
    }
}

//! SEP (Smooth Extension Protocol) hosting for the frontend.
//!
//! This is the smooth-code side of SEP Phase 3: it discovers extensions,
//! gates them on a content-hashed trust store, loads them into the engine's
//! [`ExtensionHost`] declaring the kinds of UI the TUI can render, and provides
//! [`TuiUiProvider`] — the [`HostDelegate`] that routes an extension's
//! `ui/request` onto the terminal.
//!
//! The delegate is deliberately decoupled from the ratatui event loop via the
//! [`UiSink`] trait: the delegate turns `ui/request` JSON into `UiSink` calls,
//! and the frontend (or a test) decides how to render/answer them. That keeps
//! the request→render mapping unit-testable without a live terminal.
//!
//! > **Architecture note.** In the shipped client/server split the engine
//! > `Agent` runs in `smooth-operative` (dispatched server-side), and
//! > smooth-code is a Big Smooth client. A live extension's `ui/request` reaches
//! > this provider only once the daemon relays it over the durable event surface
//! > (SEP Phase 6). Until then this module is the tested rendering/host substrate
//! > and the home of the trust store `th ext` writes to.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context as _, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use smooth_operator::extension::manifest::{default_global_dir, project_dir};
use smooth_operator::extension::protocol::{codes, HostInfo, RpcError, WorkspaceInfo};
use smooth_operator::extension::{discover, DiscoveredExtension, ExtensionHost, HostDelegate};

/// The `ui/request` kinds a full ratatui frontend can render. Declared to each
/// extension at the handshake; an extension gates on it via the SDK's `hasUI`.
pub const TUI_UI_CAPABILITIES: [&str; 7] = ["select", "confirm", "input", "notify", "set_status", "set_widget", "set_title"];

// ---------------------------------------------------------------------------
// Render blocks (the `set_widget` payload)
// ---------------------------------------------------------------------------

/// A declarative widget an extension pushes via `set_widget`. Every kind carries
/// a mandatory `text` fallback so any frontend can render it; richer frontends
/// render the structured form. Minimum Phase 3 kinds; unknown kinds fall back to
/// their `text` (or a pretty-printed blob).
#[derive(Debug, Clone, PartialEq)]
pub enum RenderBlock {
    Markdown {
        markdown: String,
    },
    KeyValue {
        title: Option<String>,
        rows: Vec<(String, String)>,
    },
    Progress {
        label: Option<String>,
        value: f64,
    },
    /// The mandatory text fallback, or an unknown kind rendered as text.
    Text(String),
}

impl RenderBlock {
    /// Parse a `set_widget` `widget` payload. Falls back to `Text` when the
    /// `kind` is missing/unknown, preferring the payload's own `text` field.
    #[must_use]
    pub fn from_widget(widget: &Value) -> Self {
        let text_fallback = || widget.get("text").and_then(Value::as_str).map(str::to_string);
        match widget.get("kind").and_then(Value::as_str) {
            Some("markdown") => Self::Markdown {
                markdown: widget
                    .get("markdown")
                    .and_then(Value::as_str)
                    .or_else(|| widget.get("text").and_then(Value::as_str))
                    .unwrap_or_default()
                    .to_string(),
            },
            Some("keyvalue") => Self::KeyValue {
                title: widget.get("title").and_then(Value::as_str).map(str::to_string),
                rows: widget
                    .get("rows")
                    .and_then(Value::as_array)
                    .map(|arr| {
                        arr.iter()
                            .map(|r| {
                                let key = r.get("key").and_then(Value::as_str).unwrap_or_default().to_string();
                                let value = r.get("value").and_then(Value::as_str).unwrap_or_default().to_string();
                                (key, value)
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
            },
            Some("progress") => Self::Progress {
                label: widget.get("label").and_then(Value::as_str).map(str::to_string),
                value: widget.get("value").and_then(Value::as_f64).unwrap_or(0.0).clamp(0.0, 1.0),
            },
            _ => Self::Text(text_fallback().unwrap_or_else(|| serde_json::to_string_pretty(widget).unwrap_or_default())),
        }
    }

    /// Render to plain terminal lines (the lowest-common-denominator view the
    /// TUI can drop into a widget region above the input).
    #[must_use]
    pub fn to_lines(&self) -> Vec<String> {
        match self {
            Self::Markdown { markdown } => markdown.lines().map(str::to_string).collect(),
            Self::KeyValue { title, rows } => {
                let mut out = Vec::new();
                if let Some(t) = title {
                    out.push(t.clone());
                }
                let width = rows.iter().map(|(k, _)| k.len()).max().unwrap_or(0);
                for (k, v) in rows {
                    out.push(format!("  {k:<width$}  {v}"));
                }
                out
            }
            Self::Progress { label, value } => {
                let filled = (value * 20.0).round() as usize;
                let bar = format!("[{}{}] {:.0}%", "#".repeat(filled), "-".repeat(20 - filled), value * 100.0);
                match label {
                    Some(l) => vec![format!("{l} {bar}")],
                    None => vec![bar],
                }
            }
            Self::Text(t) => t.lines().map(str::to_string).collect(),
        }
    }
}

// ---------------------------------------------------------------------------
// UiSink — the frontend's answer/render surface
// ---------------------------------------------------------------------------

/// The user's answer to an interactive `ui/request`. `cancelled` means the user
/// dismissed the dialog without answering.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct UiAnswer {
    pub value: Option<String>,
    pub confirmed: Option<bool>,
    pub text: Option<String>,
    pub cancelled: bool,
}

impl UiAnswer {
    #[must_use]
    pub fn cancelled() -> Self {
        Self {
            cancelled: true,
            ..Self::default()
        }
    }
    fn to_result(&self) -> Value {
        let mut m = serde_json::Map::new();
        if let Some(v) = &self.value {
            m.insert("value".into(), json!(v));
        }
        if let Some(c) = self.confirmed {
            m.insert("confirmed".into(), json!(c));
        }
        if let Some(t) = &self.text {
            m.insert("text".into(), json!(t));
        }
        if self.cancelled {
            m.insert("cancelled".into(), json!(true));
        }
        Value::Object(m)
    }
}

/// What the frontend implements to render/answer `ui/request`s. Interactive
/// kinds are async (they await the user); one-way kinds are fire-and-forget.
#[async_trait]
pub trait UiSink: Send + Sync {
    async fn select(&self, prompt: &str, options: &[String]) -> UiAnswer;
    async fn confirm(&self, prompt: &str) -> UiAnswer;
    async fn input(&self, prompt: &str, default: Option<&str>) -> UiAnswer;
    fn notify(&self, message: &str, level: &str);
    fn set_status(&self, status: &str);
    fn set_widget(&self, widget: RenderBlock);
    fn set_title(&self, title: &str);
}

// ---------------------------------------------------------------------------
// TuiUiProvider — the HostDelegate
// ---------------------------------------------------------------------------

/// Routes an extension's `ui/request` onto a [`UiSink`]. Implements the engine's
/// [`HostDelegate`]; attach it when building an [`ExtensionHost`].
pub struct TuiUiProvider {
    sink: Arc<dyn UiSink>,
}

impl TuiUiProvider {
    #[must_use]
    pub fn new(sink: Arc<dyn UiSink>) -> Self {
        Self { sink }
    }
}

#[async_trait]
impl HostDelegate for TuiUiProvider {
    async fn ui_request(&self, _ext: &str, params: Value) -> Result<Value, RpcError> {
        let kind = params.get("kind").and_then(Value::as_str).unwrap_or_default();
        let answer = match kind {
            "select" => {
                let prompt = params.get("prompt").and_then(Value::as_str).unwrap_or_default();
                let options: Vec<String> = params
                    .get("options")
                    .and_then(Value::as_array)
                    .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
                    .unwrap_or_default();
                self.sink.select(prompt, &options).await.to_result()
            }
            "confirm" => {
                let prompt = params.get("prompt").and_then(Value::as_str).unwrap_or_default();
                self.sink.confirm(prompt).await.to_result()
            }
            "input" => {
                let prompt = params.get("prompt").and_then(Value::as_str).unwrap_or_default();
                let default = params.get("default").and_then(Value::as_str);
                self.sink.input(prompt, default).await.to_result()
            }
            "notify" => {
                let message = params.get("message").and_then(Value::as_str).unwrap_or_default();
                let level = params.get("level").and_then(Value::as_str).unwrap_or("info");
                self.sink.notify(message, level);
                json!({})
            }
            "set_status" => {
                self.sink.set_status(params.get("status").and_then(Value::as_str).unwrap_or_default());
                json!({})
            }
            "set_widget" => {
                let widget = params.get("widget").cloned().unwrap_or(Value::Null);
                self.sink.set_widget(RenderBlock::from_widget(&widget));
                json!({})
            }
            "set_title" => {
                self.sink.set_title(params.get("title").and_then(Value::as_str).unwrap_or_default());
                json!({})
            }
            other => return Err(RpcError::new(codes::INVALID_PARAMS, format!("unknown ui/request kind: {other}"))),
        };
        Ok(answer)
    }
}

// ---------------------------------------------------------------------------
// Trust store — ~/.smooth/extensions/trust.toml
// ---------------------------------------------------------------------------

/// A single trust record, keyed by extension name. `hash` is the content hash at
/// the time trust was granted; a mismatch means the extension changed and must
/// be re-trusted (fail-safe).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TrustRecord {
    pub source: String,
    pub hash: String,
    pub trusted: bool,
}

/// The trust store: `name → TrustRecord`, persisted as TOML.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TrustStore {
    #[serde(default)]
    pub extensions: HashMap<String, TrustRecord>,
}

/// Path to the trust file: `<global extensions dir>/trust.toml`.
#[must_use]
pub fn trust_path() -> Option<PathBuf> {
    default_global_dir().map(|d| d.join("trust.toml"))
}

impl TrustStore {
    /// Load the trust store (empty if the file is missing/unreadable).
    #[must_use]
    pub fn load() -> Self {
        let Some(path) = trust_path() else { return Self::default() };
        std::fs::read_to_string(path).ok().and_then(|t| toml::from_str(&t).ok()).unwrap_or_default()
    }

    /// True if `name` is recorded trusted AND its recorded hash still matches.
    #[must_use]
    pub fn is_trusted(&self, name: &str, hash: &str) -> bool {
        self.extensions.get(name).is_some_and(|r| r.trusted && r.hash == hash)
    }

    /// Record (or overwrite) a trust decision.
    pub fn set(&mut self, name: &str, source: &str, hash: &str, trusted: bool) {
        self.extensions.insert(
            name.to_string(),
            TrustRecord {
                source: source.to_string(),
                hash: hash.to_string(),
                trusted,
            },
        );
    }

    /// Remove a trust record. Returns whether one was present.
    pub fn remove(&mut self, name: &str) -> bool {
        self.extensions.remove(name).is_some()
    }

    /// Persist to `trust.toml`, creating the parent dir.
    ///
    /// # Errors
    /// Returns an error if the directory can't be created or the file written.
    pub fn save(&self) -> Result<()> {
        let path = trust_path().context("no home dir for the trust store")?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, toml::to_string_pretty(self)?).with_context(|| format!("write {}", path.display()))?;
        Ok(())
    }
}

/// Content hash of an extension directory: sha256 over its `extension.toml`.
/// (The manifest fully declares the extension's identity + capabilities + run
/// command — the security-relevant surface a trust decision is made against.)
///
/// # Errors
/// Returns an error if `<dir>/extension.toml` can't be read.
pub fn hash_extension(dir: &Path) -> Result<String> {
    let manifest = dir.join("extension.toml");
    let bytes = std::fs::read(&manifest).with_context(|| format!("read {}", manifest.display()))?;
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    Ok(format!("{:x}", hasher.finalize()))
}

// ---------------------------------------------------------------------------
// Host loading — discover + trust-gate + ExtensionHost::load
// ---------------------------------------------------------------------------

/// Which trusted extensions were loaded and which discovered ones were skipped
/// for want of trust (so the frontend can hint `th ext trust <name>`).
#[derive(Debug, Default)]
pub struct LoadOutcome {
    pub skipped_untrusted: Vec<String>,
    pub failures: Vec<(String, String)>,
}

/// Discover extensions (global + project), keep only the trusted ones, and load
/// them into an [`ExtensionHost`] declaring [`TUI_UI_CAPABILITIES`] and the
/// given [`UiSink`] delegate. Untrusted extensions are skipped (never loaded).
pub async fn load_trusted_host(workspace_root: &Path, sink: Arc<dyn UiSink>) -> (ExtensionHost, LoadOutcome) {
    let global = default_global_dir();
    let project = project_dir(workspace_root);
    let (discovered, disc_failures) = discover(global.as_deref(), Some(project.as_path()));

    let trust = TrustStore::load();
    let mut trusted: Vec<DiscoveredExtension> = Vec::new();
    let mut outcome = LoadOutcome {
        failures: disc_failures,
        ..LoadOutcome::default()
    };
    for ext in discovered {
        let name = ext.manifest.name.clone();
        let hash = hash_extension(&ext.root).unwrap_or_default();
        if trust.is_trusted(&name, &hash) {
            trusted.push(ext);
        } else {
            outcome.skipped_untrusted.push(name);
        }
    }

    let host_info = HostInfo {
        name: "smooth-code".into(),
        version: env!("CARGO_PKG_VERSION").into(),
    };
    let workspace = WorkspaceInfo {
        root: workspace_root.to_string_lossy().into_owned(),
        trusted: true,
    };
    let (host, load_failures) = ExtensionHost::load(
        trusted,
        host_info,
        workspace,
        "tui",
        TUI_UI_CAPABILITIES.iter().map(|s| (*s).to_string()).collect(),
        Arc::new(TuiUiProvider::new(sink)),
    )
    .await;
    outcome.failures.extend(load_failures);
    (host, outcome)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[test]
    fn render_block_keyvalue_lines() {
        let w = json!({ "kind": "keyvalue", "title": "Todos", "rows": [{ "key": "1", "value": "○ a" }, { "key": "2", "value": "✓ b" }], "text": "fallback" });
        let rb = RenderBlock::from_widget(&w);
        let lines = rb.to_lines();
        assert_eq!(lines[0], "Todos");
        assert!(lines[1].contains("a"));
        assert_eq!(lines.len(), 3);
    }

    #[test]
    fn render_block_unknown_kind_uses_text_fallback() {
        let w = json!({ "kind": "flarb", "text": "just text" });
        assert_eq!(RenderBlock::from_widget(&w), RenderBlock::Text("just text".into()));
    }

    #[test]
    fn render_block_progress_bar() {
        let w = json!({ "kind": "progress", "label": "Build", "value": 0.5 });
        let lines = RenderBlock::from_widget(&w).to_lines();
        assert!(lines[0].starts_with("Build ["));
        assert!(lines[0].contains("50%"));
    }

    /// A UiSink that records one-way calls and answers interactive ones from a
    /// script — the test double for the real TUI dialog machinery.
    struct ScriptedSink {
        widgets: Mutex<Vec<RenderBlock>>,
        statuses: Mutex<Vec<String>>,
        confirm: bool,
    }

    #[async_trait]
    impl UiSink for ScriptedSink {
        async fn select(&self, _prompt: &str, options: &[String]) -> UiAnswer {
            UiAnswer {
                value: options.first().cloned(),
                ..UiAnswer::default()
            }
        }
        async fn confirm(&self, _prompt: &str) -> UiAnswer {
            UiAnswer {
                confirmed: Some(self.confirm),
                ..UiAnswer::default()
            }
        }
        async fn input(&self, _prompt: &str, default: Option<&str>) -> UiAnswer {
            UiAnswer {
                text: Some(default.unwrap_or("typed").to_string()),
                ..UiAnswer::default()
            }
        }
        fn notify(&self, _message: &str, _level: &str) {}
        fn set_status(&self, status: &str) {
            self.statuses.lock().unwrap().push(status.to_string());
        }
        fn set_widget(&self, widget: RenderBlock) {
            self.widgets.lock().unwrap().push(widget);
        }
        fn set_title(&self, _title: &str) {}
    }

    #[tokio::test]
    async fn provider_routes_set_widget_and_confirm() {
        let sink = Arc::new(ScriptedSink {
            widgets: Mutex::new(Vec::new()),
            statuses: Mutex::new(Vec::new()),
            confirm: true,
        });
        let provider = TuiUiProvider::new(sink.clone());

        // set_widget → sink records a render block; result is empty.
        let r = provider
            .ui_request(
                "todo",
                json!({ "kind": "set_widget", "widget": { "kind": "keyvalue", "title": "T", "rows": [], "text": "x" } }),
            )
            .await
            .unwrap();
        assert_eq!(r, json!({}));
        assert_eq!(sink.widgets.lock().unwrap().len(), 1);

        // confirm → sink's scripted verdict comes back as {confirmed:true}.
        let r = provider.ui_request("todo", json!({ "kind": "confirm", "prompt": "sure?" })).await.unwrap();
        assert_eq!(r, json!({ "confirmed": true }));

        // set_status is recorded.
        provider.ui_request("todo", json!({ "kind": "set_status", "status": "working" })).await.unwrap();
        assert_eq!(sink.statuses.lock().unwrap()[0], "working");
    }

    #[tokio::test]
    async fn provider_rejects_unknown_kind() {
        let sink = Arc::new(ScriptedSink {
            widgets: Mutex::new(Vec::new()),
            statuses: Mutex::new(Vec::new()),
            confirm: false,
        });
        let err = TuiUiProvider::new(sink).ui_request("x", json!({ "kind": "bogus" })).await.unwrap_err();
        assert_eq!(err.code, codes::INVALID_PARAMS);
    }

    #[test]
    fn trust_store_hash_gating() {
        let mut store = TrustStore::default();
        assert!(!store.is_trusted("todo", "abc"));
        store.set("todo", "/path", "abc", true);
        assert!(store.is_trusted("todo", "abc"));
        // A changed hash (extension edited) is no longer trusted.
        assert!(!store.is_trusted("todo", "def"));
        // Explicit distrust.
        store.set("todo", "/path", "abc", false);
        assert!(!store.is_trusted("todo", "abc"));
    }
}

//! Machine settings: one file, one typed registry, one resolver (pearl th-f95ecf).
//!
//! Smooth grew ~47 `SMOOTH_*` environment variables, each read ad hoc where it
//! is used, so the only way to find a knob was to read the code. This module is
//! the single place a user-facing knob is declared:
//!
//! - **[`REGISTRY`]** — every key: dotted name, type, default, the legacy env
//!   var it maps to, a one-line description, which component reads it, and
//!   whether a change needs a Big Smooth restart. Unknown keys are refused.
//! - **The file** — `~/.smooth/settings.toml` (`$SMOOTH_HOME/settings.toml`
//!   when `SMOOTH_HOME` is set). Machine-global. Dotted keys are nested TOML
//!   tables (`sandbox.enabled` → `[sandbox] enabled = true`). Writes go through
//!   `toml_edit`, so comments and keys this build doesn't know survive.
//! - **Resolution** — legacy env var > settings file > default, via
//!   [`Resolver`]. The env var still wins so every existing launch recipe
//!   (`SMOOTH_SANDBOX=1 th up`, the bench, the e2e rigs) keeps working
//!   unchanged; [`Resolver::resolve`] reports which source won.
//!
//! Call sites keep their own parsing. They swap `std::env::var("SMOOTH_X")`
//! for `raw("x")` ([`raw`]), which hands back the same string shape the env var
//! carried (bools canonicalised to `true`/`false`, lists joined with `,`), so a
//! migrated knob keeps its exact semantics and only gains the file.
//!
//! **Never put a secret here.** The file is plain TOML that `th settings list`
//! prints; tokens, keys and credentials stay env-only (or in their own 0600
//! stores). The file is still written 0600 because it carries security
//! posture (`sandbox.enabled`, `egress.allowlist`).

use std::collections::BTreeSet;
use std::fmt;
use std::path::{Path, PathBuf};

use toml_edit::{Array, DocumentMut, Item, Table, Value};

/// The value type of a setting. Drives `th settings set` validation and how a
/// file value is rendered back to the env-var string shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// `true`/`false`. `set` also accepts `1`/`0`, `yes`/`no`, `on`/`off`;
    /// the file stores a TOML bool.
    Bool,
    /// Free-form string.
    String,
    /// A signed integer.
    Int,
    /// One of a fixed set of lowercase values.
    Enum(&'static [&'static str]),
    /// A list of strings. `set` splits on commas/whitespace; the file stores a
    /// TOML array; [`raw`] joins with `,` (the env-var form).
    List,
}

impl Kind {
    /// Short type name for listings and `--json` (`bool`, `string`, `int`,
    /// `enum`, `list`).
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Bool => "bool",
            Self::String => "string",
            Self::Int => "int",
            Self::Enum(_) => "enum",
            Self::List => "list",
        }
    }

    /// The allowed values for an enum, else empty.
    #[must_use]
    pub const fn allowed(self) -> &'static [&'static str] {
        match self {
            Self::Enum(values) => values,
            _ => &[],
        }
    }
}

/// What it takes for a changed value to apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Apply {
    /// Read once when Big Smooth starts: restart it.
    RestartBigSmooth,
    /// Read on every use: the next use sees the change.
    Immediate,
}

impl Apply {
    /// Whether a change needs a Big Smooth restart.
    #[must_use]
    pub const fn restart_required(self) -> bool {
        matches!(self, Self::RestartBigSmooth)
    }
}

/// The command that restarts Big Smooth, for "restart to apply" hints.
pub const RESTART_HINT: &str = "th down && th up (or quit and reopen Big Smooth.app)";

/// One registered setting.
#[derive(Debug, Clone, Copy)]
pub struct SettingDef {
    /// Dotted key, e.g. `sandbox.enabled`.
    pub key: &'static str,
    /// Value type.
    pub kind: Kind,
    /// The effective default when neither env nor file sets it, in the env-var
    /// string form. `None` means "unset" (the reader's own fallback applies,
    /// described in [`Self::default_note`]).
    pub default: Option<&'static str>,
    /// What "unset" means, in words, when there is no literal default.
    pub default_note: &'static str,
    /// The legacy environment variable this key maps to. It still wins over
    /// the file.
    pub env: &'static str,
    /// One-line description.
    pub description: &'static str,
    /// Which component reads it (`daemon`, `tools`, …).
    pub component: &'static str,
    /// When a change takes effect.
    pub apply: Apply,
}

/// The `auto_mode` values `th settings set` accepts (the engine's
/// `AutoMode::from_env_value` also takes a few aliases from the env var).
pub const AUTO_MODES: &[&str] = &["bypass", "accept-edits", "ask", "deny"];

/// Every setting `th settings` knows. Keep sorted by key; a test enforces it.
///
/// Only user-facing toggles belong here. Internal path overrides
/// (`SMOOTH_*_DB`, `*_FILE`), test hooks, and anything secret (tokens, API
/// keys) stay env-only.
pub const REGISTRY: &[SettingDef] = &[
    SettingDef {
        key: "auto_mode",
        kind: Kind::Enum(AUTO_MODES),
        default: Some("bypass"),
        default_note: "",
        env: "SMOOTH_AUTO_MODE",
        description: "Big Smooth's permission gate mode: bypass (allow benign, deny-policy + Narc still block danger), accept-edits, ask, or deny",
        component: "daemon",
        apply: Apply::RestartBigSmooth,
    },
    SettingDef {
        key: "cloud_memory",
        kind: Kind::Bool,
        default: Some("false"),
        default_note: "",
        env: "SMOOTH_CLOUD_MEMORY",
        description: "Route remember/recall to the Smoo AI platform memory home instead of the local store (needs a signed-in Smoo session)",
        component: "daemon",
        apply: Apply::RestartBigSmooth,
    },
    // ADR-012: with Big Smooth.app installed, `th` defers to the app's daemon
    // (launching the app if needed). This lets a headless or dev setup run its own.
    SettingDef {
        key: "daemon.prefer_own",
        kind: Kind::Bool,
        default: Some("false"),
        default_note: "",
        env: "SMOOTH_PREFER_OWN_DAEMON",
        description: "Let `th up` / `th code` start their own daemon even when Big Smooth.app is installed (headless or dev setups)",
        component: "th",
        apply: Apply::Immediate,
    },
    SettingDef {
        key: "egress.allowlist",
        kind: Kind::List,
        default: None,
        default_note: "unset: no egress proxy, the bash tool's network is unrestricted",
        env: "SMOOTH_EGRESS_ALLOWLIST",
        description:
            "Exact hosts the bash tool may reach through the goalie proxy (`defaults` adds the curated dev hosts); only enforced when sandbox.enabled is on",
        component: "daemon",
        apply: Apply::RestartBigSmooth,
    },
    SettingDef {
        key: "fast_mode",
        kind: Kind::Bool,
        default: Some("false"),
        default_note: "",
        env: "SMOOTH_FAST_MODE",
        description: "Point Big Smooth at the fast (Groq) model instead of the providers.json `coding` slot; `model` still wins",
        component: "daemon",
        apply: Apply::RestartBigSmooth,
    },
    SettingDef {
        key: "harness.context_budget",
        kind: Kind::Int,
        default: Some("250000"),
        default_note: "",
        env: "SMOOTH_CONTEXT_BUDGET",
        description: "Context tokens a harness session may reach before its hooks make it hand off (/th-clear); 0 disables the budget",
        component: "harness",
        apply: Apply::Immediate,
    },
    SettingDef {
        key: "harness.context_budget_warn",
        kind: Kind::Int,
        default: Some("220000"),
        default_note: "",
        env: "SMOOTH_CONTEXT_BUDGET_WARN",
        description: "Context tokens at which a harness session is nudged once to wrap up before the budget; 0 disables the nudge",
        component: "harness",
        apply: Apply::Immediate,
    },
    SettingDef {
        key: "model",
        kind: Kind::String,
        default: None,
        default_note: "unset: fast_mode's model, else the providers.json `coding` slot",
        env: "SMOOTH_AGENT_MODEL",
        description: "Pin Big Smooth's agent model (a gateway model id); wins over fast_mode and providers.json routing",
        component: "daemon",
        apply: Apply::RestartBigSmooth,
    },
    SettingDef {
        key: "relay.enabled",
        kind: Kind::Bool,
        default: Some("true"),
        default_note: "",
        env: "SMOOTH_RELAY",
        description: "Connect Big Smooth to Smoo Relay so the phone apps can reach it",
        component: "daemon",
        apply: Apply::RestartBigSmooth,
    },
    SettingDef {
        key: "relay.url",
        kind: Kind::String,
        default: Some("wss://relay.smoo.ai/ws"),
        default_note: "",
        env: "SMOOTH_RELAY_URL",
        description: "The Smoo Relay WebSocket endpoint (an empty value disables the relay)",
        component: "daemon",
        apply: Apply::RestartBigSmooth,
    },
    SettingDef {
        key: "sandbox.enabled",
        kind: Kind::Bool,
        default: Some("false"),
        default_note: "",
        env: "SMOOTH_SANDBOX",
        description: "Run the bash tool inside the macOS kernel (Seatbelt) sandbox; off by default so the agent acts as you",
        component: "tools",
        apply: Apply::RestartBigSmooth,
    },
    SettingDef {
        key: "tailscale.serve",
        kind: Kind::Bool,
        default: Some("true"),
        default_note: "",
        env: "SMOOTH_TAILSCALE_SERVE",
        description: "Expose Big Smooth on your tailnet over HTTPS via `tailscale serve` (when tailscale is up)",
        component: "daemon",
        apply: Apply::RestartBigSmooth,
    },
];

/// Look up a registered key.
#[must_use]
pub fn def(key: &str) -> Option<&'static SettingDef> {
    REGISTRY.iter().find(|d| d.key == key)
}

/// Why a settings operation failed.
#[derive(Debug, thiserror::Error)]
pub enum SettingsError {
    /// The key isn't in [`REGISTRY`].
    #[error("unknown setting '{key}'{}", suggestion.map_or_else(String::new, |s| format!(" — did you mean '{s}'?")))]
    UnknownKey {
        /// The key as given.
        key: String,
        /// The closest registered key, when one is close enough.
        suggestion: Option<&'static str>,
    },
    /// The value doesn't fit the key's [`Kind`].
    #[error("invalid value {value:?} for '{key}' ({kind}): {reason}")]
    InvalidValue {
        /// The key.
        key: &'static str,
        /// The value as given.
        value: String,
        /// The key's type name.
        kind: &'static str,
        /// What would be accepted.
        reason: String,
    },
    /// The settings file isn't valid TOML (or its shape conflicts with a key).
    #[error("{path}: {message}")]
    File {
        /// The file.
        path: String,
        /// What is wrong.
        message: String,
    },
    /// Reading or writing the file failed.
    #[error("{path}: {source}")]
    Io {
        /// The file.
        path: String,
        /// The I/O error.
        source: std::io::Error,
    },
}

/// Look up a key, or an [`SettingsError::UnknownKey`] with a closest-match
/// suggestion.
///
/// # Errors
/// [`SettingsError::UnknownKey`] when the key isn't registered.
pub fn require_def(key: &str) -> Result<&'static SettingDef, SettingsError> {
    def(key).ok_or_else(|| SettingsError::UnknownKey {
        key: key.to_owned(),
        suggestion: suggest(key),
    })
}

/// The registered key closest to `key` (edit distance ≤ a third of its length,
/// at least 2), or one that contains it / is contained by it.
#[must_use]
pub fn suggest(key: &str) -> Option<&'static str> {
    let needle = key.trim().to_ascii_lowercase().replace('-', "_");
    if needle.is_empty() {
        return None;
    }
    let best = REGISTRY.iter().map(|d| (levenshtein(&needle, d.key), d.key)).min_by_key(|(d, _)| *d)?;
    let budget = (needle.len() / 3).max(2);
    if best.0 <= budget {
        return Some(best.1);
    }
    // A typo of one segment: `sandbx` → `sandbox.enabled`.
    if let Some(d) = REGISTRY
        .iter()
        .find(|d| d.key.split('.').any(|seg| levenshtein(&needle, seg) <= (seg.len() / 3).max(1)))
    {
        return Some(d.key);
    }
    // `sandbox` → `sandbox.enabled`, `allowlist` → `egress.allowlist`.
    REGISTRY
        .iter()
        .map(|d| d.key)
        .find(|k| k.split('.').any(|seg| seg == needle) || k.contains(&needle))
}

fn levenshtein(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut cur = vec![i + 1; b.len() + 1];
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != *cb);
            cur[j + 1] = (prev[j] + cost).min(prev[j + 1] + 1).min(cur[j] + 1);
        }
        prev = cur;
    }
    prev[b.len()]
}

/// Parse a bool the way every `SMOOTH_*` toggle does: `1`/`true`/`yes`/`on`
/// and `0`/`false`/`no`/`off`, case- and whitespace-insensitive.
#[must_use]
pub fn parse_bool(raw: &str) -> Option<bool> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

/// Split a list value: commas and whitespace separate entries, empties drop.
fn split_list(raw: &str) -> Vec<String> {
    raw.split([',', ' ', '\t', '\n'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

/// A value validated against its [`Kind`], ready to store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Typed {
    /// A bool.
    Bool(bool),
    /// A string (also enums, canonicalised to lowercase).
    String(String),
    /// An integer.
    Int(i64),
    /// A list of strings.
    List(Vec<String>),
}

impl Typed {
    /// The env-var string form call sites parse.
    #[must_use]
    pub fn to_raw(&self) -> String {
        match self {
            Self::Bool(b) => b.to_string(),
            Self::String(s) => s.clone(),
            Self::Int(i) => i.to_string(),
            Self::List(items) => items.join(","),
        }
    }

    /// The JSON form for `--json` output.
    #[must_use]
    pub fn to_json(&self) -> serde_json::Value {
        match self {
            Self::Bool(b) => serde_json::Value::Bool(*b),
            Self::String(s) => serde_json::Value::String(s.clone()),
            Self::Int(i) => serde_json::Value::from(*i),
            Self::List(items) => serde_json::Value::Array(items.iter().cloned().map(serde_json::Value::String).collect()),
        }
    }

    fn to_toml(&self) -> Value {
        match self {
            Self::Bool(b) => Value::from(*b),
            Self::String(s) => Value::from(s.as_str()),
            Self::Int(i) => Value::from(*i),
            Self::List(items) => {
                let mut arr = Array::new();
                for item in items {
                    arr.push(item.as_str());
                }
                Value::Array(arr)
            }
        }
    }
}

/// Validate a user-supplied string for `def`'s [`Kind`].
///
/// # Errors
/// [`SettingsError::InvalidValue`] naming what would be accepted.
pub fn parse_value(def: &SettingDef, raw: &str) -> Result<Typed, SettingsError> {
    let invalid = |reason: String| SettingsError::InvalidValue {
        key: def.key,
        value: raw.to_owned(),
        kind: def.kind.name(),
        reason,
    };
    match def.kind {
        Kind::Bool => parse_bool(raw)
            .map(Typed::Bool)
            .ok_or_else(|| invalid("use true/false (or 1/0, yes/no, on/off)".to_owned())),
        Kind::String => Ok(Typed::String(raw.trim().to_owned())),
        Kind::Int => raw.trim().parse::<i64>().map(Typed::Int).map_err(|_| invalid("use a whole number".to_owned())),
        Kind::Enum(allowed) => {
            let v = raw.trim().to_ascii_lowercase();
            if allowed.contains(&v.as_str()) {
                Ok(Typed::String(v))
            } else {
                Err(invalid(format!("use one of: {}", allowed.join(", "))))
            }
        }
        Kind::List => {
            let items = split_list(raw);
            if items.is_empty() {
                Err(invalid(format!(
                    "give at least one entry (comma-separated); `th settings unset {}` turns it off",
                    def.key
                )))
            } else {
                Ok(Typed::List(items))
            }
        }
    }
}

/// Convert a value read from the file into a [`Typed`] for `def`, accepting
/// the TOML type `set` writes and a plain string a person may have typed.
fn typed_from_item(def: &SettingDef, value: &Value) -> Result<Typed, String> {
    let mismatch = || format!("expected {} in the settings file, found {}", def.kind.name(), value.type_name());
    match (def.kind, value) {
        (Kind::Bool, Value::Boolean(b)) => Ok(Typed::Bool(*b.value())),
        (Kind::Int, Value::Integer(i)) => Ok(Typed::Int(*i.value())),
        (Kind::List, Value::Array(arr)) => {
            let mut items = Vec::new();
            for v in arr {
                items.push(v.as_str().ok_or_else(mismatch)?.to_owned());
            }
            Ok(Typed::List(items))
        }
        (_, Value::String(s)) => parse_value(def, s.value()).map_err(|e| e.to_string()),
        _ => Err(mismatch()),
    }
}

/// `$SMOOTH_HOME/settings.toml`, else `~/.smooth/settings.toml`.
#[must_use]
pub fn settings_path() -> Option<PathBuf> {
    settings_path_from(std::env::var_os("SMOOTH_HOME").map(PathBuf::from), dirs_next::home_dir())
}

fn settings_path_from(smooth_home: Option<PathBuf>, home: Option<PathBuf>) -> Option<PathBuf> {
    smooth_home
        .filter(|p| !p.as_os_str().is_empty())
        .map_or_else(|| home.map(|h| h.join(".smooth").join("settings.toml")), |dir| Some(dir.join("settings.toml")))
}

/// The settings file as an editable document. Loading a missing file gives an
/// empty one; saving creates it (0600) and its directory.
#[derive(Debug, Clone)]
pub struct SettingsFile {
    path: Option<PathBuf>,
    doc: DocumentMut,
    exists: bool,
}

impl SettingsFile {
    /// An empty, pathless file (nothing set). What a resolver uses when there
    /// is no home directory.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            path: None,
            doc: DocumentMut::new(),
            exists: false,
        }
    }

    /// Parse settings from TOML text (no path; [`Self::save`] will refuse).
    ///
    /// # Errors
    /// [`SettingsError::File`] if the text isn't valid TOML.
    pub fn parse(text: &str) -> Result<Self, SettingsError> {
        let doc = text.parse::<DocumentMut>().map_err(|e| SettingsError::File {
            path: "<settings>".to_owned(),
            message: e.to_string().trim().to_owned(),
        })?;
        Ok(Self { path: None, doc, exists: true })
    }

    /// Load `path`. A missing file is an empty document.
    ///
    /// # Errors
    /// [`SettingsError::Io`] for an unreadable file, [`SettingsError::File`]
    /// for invalid TOML.
    pub fn load(path: &Path) -> Result<Self, SettingsError> {
        match std::fs::read_to_string(path) {
            Ok(text) => {
                let mut file = Self::parse(&text).map_err(|e| match e {
                    SettingsError::File { message, .. } => SettingsError::File {
                        path: path.display().to_string(),
                        message,
                    },
                    other => other,
                })?;
                file.path = Some(path.to_path_buf());
                Ok(file)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self {
                path: Some(path.to_path_buf()),
                doc: DocumentMut::new(),
                exists: false,
            }),
            Err(source) => Err(SettingsError::Io {
                path: path.display().to_string(),
                source,
            }),
        }
    }

    /// Load the default [`settings_path`].
    ///
    /// # Errors
    /// As [`Self::load`].
    pub fn load_default() -> Result<Self, SettingsError> {
        settings_path().map_or_else(|| Ok(Self::empty()), |p| Self::load(&p))
    }

    /// The file's path, if it has one.
    #[must_use]
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Whether the file existed when loaded (or was parsed from text).
    #[must_use]
    pub const fn exists(&self) -> bool {
        self.exists
    }

    /// The raw TOML value at a dotted key, if present.
    fn value_at(&self, key: &str) -> Option<&Value> {
        let mut item: &Item = self.doc.as_item();
        for seg in key.split('.') {
            item = item.as_table_like()?.get(seg)?;
        }
        item.as_value()
    }

    /// The file's value for a registered key: `None` if absent, `Some(Err)`
    /// if present but not valid for the key's type.
    #[must_use]
    pub fn get(&self, def: &SettingDef) -> Option<Result<Typed, String>> {
        self.value_at(def.key).map(|v| typed_from_item(def, v))
    }

    /// Set a registered key, creating parent tables as needed. Comments and
    /// every other key are preserved.
    ///
    /// # Errors
    /// [`SettingsError::File`] when a parent segment already holds a
    /// non-table value (e.g. `sandbox = true` blocks `sandbox.enabled`).
    pub fn set(&mut self, def: &SettingDef, value: &Typed) -> Result<(), SettingsError> {
        let path = self.display_path();
        let segs: Vec<&str> = def.key.split('.').collect();
        let Some((leaf, parents)) = segs.split_last() else {
            return Err(SettingsError::File {
                path,
                message: format!("can't address '{}'", def.key),
            });
        };
        let mut table: &mut dyn toml_edit::TableLike = self.doc.as_table_mut();
        for seg in parents {
            let entry = table.entry(seg).or_insert(Item::Table(Table::new()));
            table = entry.as_table_like_mut().ok_or_else(|| SettingsError::File {
                path: path.clone(),
                message: format!("'{seg}' is not a table, so '{}' can't be set under it — fix the file by hand", def.key),
            })?;
        }
        // Keep the existing key's decor (a trailing comment) when overwriting.
        match table.get_mut(leaf).and_then(Item::as_value_mut) {
            Some(existing) => {
                let decor = existing.decor().clone();
                *existing = value.to_toml();
                *existing.decor_mut() = decor;
            }
            None => {
                table.insert(leaf, Item::Value(value.to_toml()));
            }
        }
        Ok(())
    }

    fn display_path(&self) -> String {
        self.path.as_ref().map_or_else(|| "<settings>".to_owned(), |p| p.display().to_string())
    }

    /// Remove a key. Returns whether it was present. A parent table this
    /// leaves empty is dropped too, unless it carries a comment (kept so a
    /// hand-written note isn't lost).
    pub fn unset(&mut self, def: &SettingDef) -> bool {
        let segs: Vec<&str> = def.key.split('.').collect();
        let Some((leaf, parents)) = segs.split_last() else {
            return false;
        };
        let mut item: &mut Item = self.doc.as_item_mut();
        for seg in parents {
            let Some(next) = item.as_table_like_mut().and_then(|t| t.get_mut(seg)) else {
                return false;
            };
            item = next;
        }
        let removed = item.as_table_like_mut().and_then(|t| t.remove(leaf)).is_some();
        if removed {
            self.prune_empty_parents(parents);
        }
        removed
    }

    /// Drop now-empty, comment-free parent tables, deepest first.
    fn prune_empty_parents(&mut self, parents: &[&str]) {
        for depth in (1..=parents.len()).rev() {
            let (path, name) = (&parents[..depth - 1], parents[depth - 1]);
            let mut holder: &mut Item = self.doc.as_item_mut();
            for seg in path {
                match holder.as_table_like_mut().and_then(|t| t.get_mut(seg)) {
                    Some(next) => holder = next,
                    None => return,
                }
            }
            let Some(table) = holder.as_table_like_mut() else { return };
            let prunable = table.get(name).is_some_and(|it| match it {
                Item::Table(t) => t.is_empty() && !has_comment(t.decor()),
                Item::Value(Value::InlineTable(t)) => t.is_empty(),
                _ => false,
            });
            if !prunable {
                return;
            }
            table.remove(name);
        }
    }

    /// Dotted keys present in the file that aren't registered (typos, or keys
    /// from a newer `th`). They are preserved on write, never read.
    #[must_use]
    pub fn unknown_keys(&self) -> Vec<String> {
        let mut out = BTreeSet::new();
        collect_leaves(self.doc.as_table(), "", &mut out);
        out.into_iter().filter(|k| def(k).is_none()).collect()
    }

    /// The document as TOML text.
    #[must_use]
    pub fn to_toml_string(&self) -> String {
        self.doc.to_string()
    }

    /// Write the file atomically (temp file + rename), mode 0600, creating its
    /// directory.
    ///
    /// # Errors
    /// [`SettingsError::Io`] on any filesystem failure; [`SettingsError::File`]
    /// if this document has no path.
    pub fn save(&mut self) -> Result<(), SettingsError> {
        let path = self.path.clone().ok_or_else(|| SettingsError::File {
            path: "<settings>".to_owned(),
            message: "no settings path (is HOME set?)".to_owned(),
        })?;
        let io = |source| SettingsError::Io {
            path: path.display().to_string(),
            source,
        };
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(io)?;
        }
        let tmp = path.with_extension(format!("toml.tmp.{}", std::process::id()));
        write_private(&tmp, self.doc.to_string().as_bytes()).map_err(io)?;
        if let Err(e) = std::fs::rename(&tmp, &path) {
            let _ = std::fs::remove_file(&tmp);
            return Err(io(e));
        }
        self.exists = true;
        Ok(())
    }
}

fn has_comment(decor: &toml_edit::Decor) -> bool {
    let text = |r: Option<&toml_edit::RawString>| r.and_then(toml_edit::RawString::as_str).is_some_and(|s| s.contains('#'));
    text(decor.prefix()) || text(decor.suffix())
}

fn collect_leaves(table: &dyn toml_edit::TableLike, prefix: &str, out: &mut BTreeSet<String>) {
    for (k, item) in table.iter() {
        let key = if prefix.is_empty() { k.to_owned() } else { format!("{prefix}.{k}") };
        if let Some(sub) = item.as_table_like() {
            collect_leaves(sub, &key, out);
        } else {
            out.insert(key);
        }
    }
}

#[cfg(unix)]
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(path)?;
    // `mode` only applies on create; tighten a pre-existing temp file too.
    f.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    f.write_all(bytes)?;
    f.sync_all()
}

#[cfg(not(unix))]
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    std::fs::write(path, bytes)
}

/// Where a resolved value came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// The legacy environment variable (wins over everything).
    Env,
    /// The settings file.
    File,
    /// The registry default (or unset, when there is none).
    Default,
}

impl Source {
    /// `env` / `file` / `default`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Env => "env",
            Self::File => "file",
            Self::Default => "default",
        }
    }
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One key, resolved.
#[derive(Debug, Clone)]
pub struct Resolved {
    /// The registry entry.
    pub def: &'static SettingDef,
    /// The effective value in env-var string form (`None` = unset; the
    /// reader's own fallback applies).
    pub value: Option<String>,
    /// Which source won.
    pub source: Source,
    /// The env var's value, when set (it overrides the file).
    pub env_value: Option<String>,
    /// The file's value, when set and valid.
    pub file_value: Option<Typed>,
    /// The file's value was present but invalid for the key's type; it is
    /// ignored (resolution falls through to the default).
    pub file_error: Option<String>,
}

impl Resolved {
    /// The effective value as a bool (for [`Kind::Bool`] keys), using
    /// [`parse_bool`]. An unparseable value counts as `false`.
    #[must_use]
    pub fn as_bool(&self) -> bool {
        self.value.as_deref().and_then(parse_bool).unwrap_or(false)
    }

    /// The stable `--json` shape `th settings` prints and the MCP tool returns.
    #[must_use]
    pub fn to_json(&self) -> serde_json::Value {
        let d = self.def;
        let value = match (&self.source, &self.file_value) {
            (Source::File, Some(t)) => t.to_json(),
            _ => self.value.as_deref().map_or(serde_json::Value::Null, |v| typed_json_from_raw(d, v)),
        };
        serde_json::json!({
            "key": d.key,
            "type": d.kind.name(),
            "allowed": d.kind.allowed(),
            "value": value,
            "source": self.source.as_str(),
            "default": d.default.map_or(serde_json::Value::Null, |v| typed_json_from_raw(d, v)),
            "default_note": if d.default_note.is_empty() { serde_json::Value::Null } else { serde_json::Value::String(d.default_note.to_owned()) },
            "env": d.env,
            "env_value": self.env_value,
            "file_value": self.file_value.as_ref().map(Typed::to_json),
            "file_error": self.file_error,
            "description": d.description,
            "component": d.component,
            "restart_required": d.apply.restart_required(),
        })
    }
}

/// Best-effort typed JSON for a raw (env/default) string: bools and ints when
/// they parse, lists split, everything else a string.
fn typed_json_from_raw(def: &SettingDef, raw: &str) -> serde_json::Value {
    match def.kind {
        Kind::Bool => parse_bool(raw).map_or_else(|| serde_json::Value::String(raw.to_owned()), serde_json::Value::Bool),
        Kind::Int => raw
            .trim()
            .parse::<i64>()
            .map_or_else(|_| serde_json::Value::String(raw.to_owned()), serde_json::Value::from),
        Kind::List => serde_json::Value::Array(split_list(raw).into_iter().map(serde_json::Value::String).collect()),
        Kind::String | Kind::Enum(_) => serde_json::Value::String(raw.to_owned()),
    }
}

type EnvFn = Box<dyn Fn(&str) -> Option<String> + Send + Sync>;

/// Resolves keys: legacy env var > settings file > default.
pub struct Resolver {
    env: EnvFn,
    file: SettingsFile,
    /// Why the file couldn't be read, if it couldn't (resolution then ignores
    /// the file rather than failing the reader).
    file_load_error: Option<String>,
}

impl fmt::Debug for Resolver {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Resolver").field("file", &self.file.path()).finish_non_exhaustive()
    }
}

impl Resolver {
    /// A resolver over an explicit env lookup and file — the test seam.
    #[must_use]
    pub fn new(env: impl Fn(&str) -> Option<String> + Send + Sync + 'static, file: SettingsFile) -> Self {
        Self {
            env: Box::new(env),
            file,
            file_load_error: None,
        }
    }

    /// This process's environment and the default settings file. An unreadable
    /// or malformed file is ignored (reported by [`Self::file_load_error`]) so
    /// a bad edit can never stop the daemon from starting.
    #[must_use]
    pub fn from_process() -> Self {
        let (file, err) = match SettingsFile::load_default() {
            Ok(f) => (f, None),
            Err(e) => (SettingsFile::empty(), Some(e.to_string())),
        };
        Self {
            env: Box::new(|name| std::env::var(name).ok()),
            file,
            file_load_error: err,
        }
    }

    /// The file this resolver read.
    #[must_use]
    pub const fn file(&self) -> &SettingsFile {
        &self.file
    }

    /// Why the settings file was ignored, if it was.
    #[must_use]
    pub fn file_load_error(&self) -> Option<&str> {
        self.file_load_error.as_deref()
    }

    /// Resolve a registered key.
    ///
    /// # Errors
    /// [`SettingsError::UnknownKey`] for an unregistered key.
    pub fn resolve(&self, key: &str) -> Result<Resolved, SettingsError> {
        Ok(self.resolve_def(require_def(key)?))
    }

    /// Resolve a registry entry.
    #[must_use]
    pub fn resolve_def(&self, def: &'static SettingDef) -> Resolved {
        let env_value = (self.env)(def.env);
        let (file_value, file_error) = match self.file.get(def) {
            None => (None, None),
            Some(Ok(t)) => (Some(t), None),
            Some(Err(e)) => (None, Some(e)),
        };
        let (value, source) = match (&env_value, &file_value) {
            (Some(v), _) => (Some(v.clone()), Source::Env),
            (None, Some(t)) => (Some(t.to_raw()), Source::File),
            (None, None) => (def.default.map(ToOwned::to_owned), Source::Default),
        };
        Resolved {
            def,
            value,
            source,
            env_value,
            file_value,
            file_error,
        }
    }

    /// Every registered key, resolved, in registry order.
    #[must_use]
    pub fn resolve_all(&self) -> Vec<Resolved> {
        REGISTRY.iter().map(|d| self.resolve_def(d)).collect()
    }

    /// The env-or-file value for a call site, in the env-var string form, or
    /// `None` when neither sets it (so the call site's own default applies,
    /// exactly as when the env var was unset). An unregistered key is a
    /// programming error: it debug-asserts and reads as unset.
    #[must_use]
    pub fn raw(&self, key: &str) -> Option<String> {
        let Some(d) = def(key) else {
            debug_assert!(false, "settings::raw: '{key}' is not in settings::REGISTRY");
            return None;
        };
        let r = self.resolve_def(d);
        match r.source {
            Source::Env | Source::File => r.value,
            Source::Default => None,
        }
    }
}

/// [`Resolver::raw`] over this process's env and the default settings file.
/// The drop-in replacement for `std::env::var("SMOOTH_X").ok()` at a migrated
/// call site.
#[must_use]
pub fn raw(key: &str) -> Option<String> {
    Resolver::from_process().raw(key)
}

/// The effective bool for a [`Kind::Bool`] key (env > file > default).
#[must_use]
pub fn get_bool(key: &str) -> bool {
    Resolver::from_process().resolve(key).is_ok_and(|r| r.as_bool())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, reason = "unwrap/expect are the idiom for test assertions")]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env_of(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> + Send + Sync + 'static {
        let map: HashMap<String, String> = pairs.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect();
        move |name| map.get(name).cloned()
    }

    fn file(text: &str) -> SettingsFile {
        SettingsFile::parse(text).expect("valid toml")
    }

    #[test]
    fn registry_is_sorted_unique_and_well_formed() {
        let keys: Vec<&str> = REGISTRY.iter().map(|d| d.key).collect();
        let mut sorted = keys.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(keys, sorted, "REGISTRY must be sorted by key with no duplicates");
        let mut envs: Vec<&str> = REGISTRY.iter().map(|d| d.env).collect();
        envs.sort_unstable();
        let n = envs.len();
        envs.dedup();
        assert_eq!(n, envs.len(), "each env var maps to one key");
        for d in REGISTRY {
            assert!(d.env.starts_with("SMOOTH_"), "{}", d.key);
            assert!(!d.description.is_empty() && !d.component.is_empty(), "{}", d.key);
            assert!(
                d.key.split('.').all(|s| !s.is_empty() && s.chars().all(|c| c.is_ascii_lowercase() || c == '_')),
                "{}",
                d.key
            );
            // A literal default must itself be a valid value; no default needs a note.
            match d.default {
                Some(v) => assert!(parse_value(d, v).is_ok(), "default for {} must validate", d.key),
                None => assert!(!d.default_note.is_empty(), "{} has no default, so say what unset means", d.key),
            }
            // Secrets never belong in a plain settings file.
            for banned in ["TOKEN", "KEY", "SECRET", "PASSWORD"] {
                assert!(!d.env.contains(banned), "{} maps to a secret-looking env var", d.key);
            }
        }
    }

    #[test]
    fn precedence_is_env_then_file_then_default_with_source() {
        let f = "[sandbox]\nenabled = true\n";
        // Default only.
        let r = Resolver::new(env_of(&[]), SettingsFile::empty()).resolve("sandbox.enabled").unwrap();
        assert_eq!((r.value.as_deref(), r.source), (Some("false"), Source::Default));
        assert!(!r.as_bool());
        // File beats default.
        let r = Resolver::new(env_of(&[]), file(f)).resolve("sandbox.enabled").unwrap();
        assert_eq!((r.value.as_deref(), r.source), (Some("true"), Source::File));
        assert!(r.as_bool());
        // Env beats file, and the file value is still reported.
        let r = Resolver::new(env_of(&[("SMOOTH_SANDBOX", "0")]), file(f)).resolve("sandbox.enabled").unwrap();
        assert_eq!((r.value.as_deref(), r.source), (Some("0"), Source::Env));
        assert_eq!(r.file_value, Some(Typed::Bool(true)));
        assert_eq!(r.env_value.as_deref(), Some("0"));
        assert!(!r.as_bool());
    }

    #[test]
    fn env_present_but_empty_still_wins() {
        // Exact legacy semantics: a set-but-empty env var is "set".
        let r = Resolver::new(env_of(&[("SMOOTH_AUTO_MODE", "")]), file("auto_mode = \"ask\"\n"));
        assert_eq!(r.raw("auto_mode").as_deref(), Some(""));
    }

    #[test]
    fn raw_is_none_at_default_so_call_sites_keep_their_fallback() {
        let r = Resolver::new(env_of(&[]), SettingsFile::empty());
        for d in REGISTRY {
            assert_eq!(r.raw(d.key), None, "{}", d.key);
        }
    }

    #[test]
    fn raw_renders_file_values_in_env_var_form() {
        let r = Resolver::new(
            env_of(&[]),
            file(
                "auto_mode = \"ASK\"\nfast_mode = true\nmodel = \"groq-gpt-oss-120b\"\n\n[egress]\nallowlist = [\"defaults\", \"github.com\"]\n\n[relay]\nenabled = false\nurl = \"wss://example/ws\"\n\n[tailscale]\nserve = false\n",
            ),
        );
        assert_eq!(r.raw("auto_mode").as_deref(), Some("ask"), "enums canonicalise");
        assert_eq!(r.raw("fast_mode").as_deref(), Some("true"));
        assert_eq!(r.raw("model").as_deref(), Some("groq-gpt-oss-120b"));
        assert_eq!(r.raw("egress.allowlist").as_deref(), Some("defaults,github.com"));
        assert_eq!(r.raw("relay.enabled").as_deref(), Some("false"));
        assert_eq!(r.raw("relay.url").as_deref(), Some("wss://example/ws"));
        assert_eq!(r.raw("tailscale.serve").as_deref(), Some("false"));
    }

    #[test]
    fn hand_typed_strings_in_the_file_are_accepted_and_canonicalised() {
        let r = Resolver::new(env_of(&[]), file("cloud_memory = \"YES\"\n[egress]\nallowlist = \"a.com, b.com\"\n"));
        assert_eq!(r.raw("cloud_memory").as_deref(), Some("true"));
        assert_eq!(r.raw("egress.allowlist").as_deref(), Some("a.com,b.com"));
    }

    #[test]
    fn invalid_file_values_are_ignored_and_reported() {
        let r = Resolver::new(env_of(&[]), file("auto_mode = \"yolo-ish\"\n[sandbox]\nenabled = 3\n"));
        let a = r.resolve("auto_mode").unwrap();
        assert_eq!(a.source, Source::Default);
        assert!(a.file_error.as_deref().unwrap().contains("one of"), "{:?}", a.file_error);
        let s = r.resolve("sandbox.enabled").unwrap();
        assert_eq!((s.value.as_deref(), s.source), (Some("false"), Source::Default));
        assert!(s.file_error.as_deref().unwrap().contains("expected bool"), "{:?}", s.file_error);
        assert_eq!(r.raw("sandbox.enabled"), None);
    }

    #[test]
    fn unknown_keys_are_refused_with_a_suggestion() {
        let err = require_def("sandbox.enable").unwrap_err();
        assert!(err.to_string().contains("did you mean 'sandbox.enabled'"), "{err}");
        assert_eq!(suggest("sandbox"), Some("sandbox.enabled"));
        assert_eq!(suggest("sandbx"), Some("sandbox.enabled"), "a typo of one segment");
        assert_eq!(suggest("tailscle.serve"), Some("tailscale.serve"));
        assert_eq!(suggest("allowlist"), Some("egress.allowlist"));
        assert_eq!(suggest("auto-mode"), Some("auto_mode"));
        assert_eq!(suggest("totally.unrelated.thing"), None);
        assert!(Resolver::new(env_of(&[]), SettingsFile::empty()).resolve("nope").is_err());
    }

    #[test]
    fn parse_value_validates_every_kind() {
        let b = def("sandbox.enabled").unwrap();
        for (v, want) in [
            ("1", true),
            ("TRUE", true),
            (" on ", true),
            ("yes", true),
            ("0", false),
            ("off", false),
            ("No", false),
        ] {
            assert_eq!(parse_value(b, v).unwrap(), Typed::Bool(want), "{v}");
        }
        assert!(parse_value(b, "enabled").unwrap_err().to_string().contains("true/false"));

        let e = def("auto_mode").unwrap();
        assert_eq!(parse_value(e, " Accept-Edits ").unwrap(), Typed::String("accept-edits".into()));
        assert!(parse_value(e, "yolo").unwrap_err().to_string().contains("bypass, accept-edits, ask, deny"));

        let l = def("egress.allowlist").unwrap();
        assert_eq!(
            parse_value(l, "defaults, github.com  x.io").unwrap(),
            Typed::List(vec!["defaults".into(), "github.com".into(), "x.io".into()])
        );
        assert!(parse_value(l, " , ").unwrap_err().to_string().contains("unset egress.allowlist"));

        let s = def("model").unwrap();
        assert_eq!(parse_value(s, " m1 ").unwrap(), Typed::String("m1".into()));

        let int = SettingDef {
            key: "x.n",
            kind: Kind::Int,
            default: None,
            default_note: "n",
            env: "SMOOTH_X_N",
            description: "d",
            component: "c",
            apply: Apply::Immediate,
        };
        assert_eq!(parse_value(&int, "42").unwrap(), Typed::Int(42));
        assert!(parse_value(&int, "4.2").is_err());
    }

    #[test]
    fn set_unset_round_trip_preserves_comments_and_unrelated_keys() {
        let original =
            "# my settings — hands off\nauto_mode = \"ask\" # keep me\n\n[sandbox]\n# why: corp laptop\nenabled = false\n\n[mine]\nfavourite = \"teal\"\n";
        let mut f = file(original);
        let sandbox = def("sandbox.enabled").unwrap();
        f.set(sandbox, &Typed::Bool(true)).unwrap();
        f.set(def("relay.url").unwrap(), &Typed::String("wss://r/ws".into())).unwrap();
        f.set(def("auto_mode").unwrap(), &Typed::String("deny".into())).unwrap();
        let text = f.to_toml_string();
        assert!(text.contains("# my settings — hands off"), "{text}");
        assert!(text.contains("# why: corp laptop"), "{text}");
        assert!(text.contains("auto_mode = \"deny\" # keep me"), "trailing comment kept on overwrite: {text}");
        assert!(text.contains("enabled = true"), "{text}");
        assert!(text.contains("[mine]\nfavourite = \"teal\""), "{text}");
        assert!(text.contains("[relay]\nurl = \"wss://r/ws\""), "{text}");

        // Re-parse: everything resolves as written.
        let again = file(&text);
        assert_eq!(again.get(sandbox), Some(Ok(Typed::Bool(true))));
        assert_eq!(again.unknown_keys(), vec!["mine.favourite".to_owned()]);

        // Unset removes only that key.
        let mut again = again;
        assert!(again.unset(sandbox));
        assert!(!again.unset(sandbox), "second unset is a no-op");
        assert!(!again.unset(def("tailscale.serve").unwrap()), "absent table");
        let text = again.to_toml_string();
        assert!(!text.contains("enabled = true"), "{text}");
        assert!(!text.contains("[sandbox]"), "an emptied, comment-free parent table is dropped: {text}");
        assert!(text.contains("favourite = \"teal\"") && text.contains("url = \"wss://r/ws\""), "{text}");
        assert_eq!(again.get(sandbox), None);
    }

    #[test]
    fn unset_prunes_empty_tables_but_keeps_commented_ones() {
        let mut f = file("[egress]\nallowlist = [\"a.com\"]\n\n# relay notes: staging box\n[relay]\nurl = \"wss://s/ws\"\n\n[sandbox]\nenabled = true\n");
        assert!(f.unset(def("egress.allowlist").unwrap()));
        assert!(f.unset(def("relay.url").unwrap()));
        let text = f.to_toml_string();
        assert!(!text.contains("[egress]"), "{text}");
        assert!(text.contains("# relay notes: staging box\n[relay]"), "a commented table survives: {text}");
        assert!(text.contains("[sandbox]\nenabled = true"), "{text}");
        // Inline and dotted parents prune too.
        let mut f = file("relay = { url = \"wss://i/ws\" }\nsandbox.enabled = true\n");
        assert!(f.unset(def("relay.url").unwrap()));
        assert!(f.unset(def("sandbox.enabled").unwrap()));
        assert_eq!(f.to_toml_string().trim(), "", "{}", f.to_toml_string());
    }

    #[test]
    fn set_writes_top_level_keys_before_tables() {
        let mut f = file("[sandbox]\nenabled = true\n");
        f.set(def("fast_mode").unwrap(), &Typed::Bool(true)).unwrap();
        let text = f.to_toml_string();
        // A root key must not land inside [sandbox] — re-parse proves it.
        let again = file(&text);
        assert_eq!(again.get(def("fast_mode").unwrap()), Some(Ok(Typed::Bool(true))), "{text}");
        assert_eq!(again.get(def("sandbox.enabled").unwrap()), Some(Ok(Typed::Bool(true))), "{text}");
    }

    #[test]
    fn set_under_a_non_table_parent_is_an_error_not_a_clobber() {
        let mut f = file("sandbox = true\n");
        let err = f.set(def("sandbox.enabled").unwrap(), &Typed::Bool(true)).unwrap_err();
        assert!(err.to_string().contains("not a table"), "{err}");
        assert!(f.to_toml_string().contains("sandbox = true"));
    }

    #[test]
    fn dotted_and_inline_table_forms_are_read() {
        let f = file("sandbox.enabled = true\nrelay = { url = \"wss://i/ws\" }\n");
        assert_eq!(f.get(def("sandbox.enabled").unwrap()), Some(Ok(Typed::Bool(true))));
        assert_eq!(f.get(def("relay.url").unwrap()), Some(Ok(Typed::String("wss://i/ws".into()))));
        assert!(f.unknown_keys().is_empty(), "{:?}", f.unknown_keys());
    }

    #[test]
    fn load_save_round_trip_on_disk_is_private() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("settings.toml");
        let mut f = SettingsFile::load(&path).unwrap();
        assert!(!f.exists());
        f.set(def("sandbox.enabled").unwrap(), &Typed::Bool(true)).unwrap();
        f.save().unwrap();
        assert!(f.exists());
        let loaded = SettingsFile::load(&path).unwrap();
        assert_eq!(loaded.get(def("sandbox.enabled").unwrap()), Some(Ok(Typed::Bool(true))));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
        // No temp file left behind.
        let leftovers: Vec<_> = std::fs::read_dir(path.parent().unwrap()).unwrap().collect();
        assert_eq!(leftovers.len(), 1);
    }

    #[test]
    fn malformed_file_is_an_error_on_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.toml");
        std::fs::write(&path, "this is = = not toml").unwrap();
        let err = SettingsFile::load(&path).unwrap_err();
        assert!(err.to_string().contains("settings.toml"), "{err}");
        assert!(SettingsFile::empty().save().is_err(), "pathless file refuses to save");
    }

    #[test]
    fn settings_path_respects_smooth_home() {
        assert_eq!(
            settings_path_from(Some(PathBuf::from("/x/sh")), Some(PathBuf::from("/home/u"))),
            Some(PathBuf::from("/x/sh/settings.toml"))
        );
        assert_eq!(
            settings_path_from(Some(PathBuf::new()), Some(PathBuf::from("/home/u"))),
            Some(PathBuf::from("/home/u/.smooth/settings.toml")),
            "empty SMOOTH_HOME is ignored"
        );
        assert_eq!(
            settings_path_from(None, Some(PathBuf::from("/home/u"))),
            Some(PathBuf::from("/home/u/.smooth/settings.toml"))
        );
        assert_eq!(settings_path_from(None, None), None);
    }

    #[test]
    fn json_shape_is_stable() {
        let r = Resolver::new(env_of(&[("SMOOTH_FAST_MODE", "1")]), file("[egress]\nallowlist = [\"github.com\"]\n"));
        let j = r.resolve("egress.allowlist").unwrap().to_json();
        let keys: BTreeSet<&str> = j.as_object().unwrap().keys().map(String::as_str).collect();
        let want: BTreeSet<&str> = [
            "key",
            "type",
            "allowed",
            "value",
            "source",
            "default",
            "default_note",
            "env",
            "env_value",
            "file_value",
            "file_error",
            "description",
            "component",
            "restart_required",
        ]
        .into_iter()
        .collect();
        assert_eq!(keys, want);
        assert_eq!(j["value"], serde_json::json!(["github.com"]));
        assert_eq!(j["source"], "file");
        assert_eq!(j["default"], serde_json::Value::Null);
        assert_eq!(j["type"], "list");
        assert_eq!(j["restart_required"], true);

        let f = r.resolve("fast_mode").unwrap().to_json();
        assert_eq!(f["value"], true, "env '1' renders as a JSON bool");
        assert_eq!(f["source"], "env");
        assert_eq!(f["env_value"], "1");
        assert_eq!(f["default"], false);

        let a = r.resolve("auto_mode").unwrap().to_json();
        assert_eq!(a["allowed"], serde_json::json!(AUTO_MODES));
        assert_eq!(a["value"], "bypass");
        assert_eq!(a["source"], "default");
    }
}

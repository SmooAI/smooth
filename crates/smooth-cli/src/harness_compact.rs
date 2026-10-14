//! The context budget as each harness's own auto-compaction threshold
//! (SMOODEV-3891, pearl th-44a836).
//!
//! Long sessions on 1M-context models are what drive input-token spend: every
//! call resends the whole context, and native auto-compaction only fires near
//! the window (~970k). Compaction itself is cheap and cuts 80–90% of the
//! context, so the fix is to make it fire at `harness.context_budget`:
//!
//! - **Budgets** — `harness.context_budget` (global), overridden per harness
//!   and/or model by `harness.context_budgets` (`selector=tokens`, most
//!   specific wins). The warn threshold keeps the global gap below the budget.
//! - **Native thresholds** — `th harness enable` writes the harness-level
//!   budget into Claude Code (`autoCompactWindow` in `~/.claude/settings.json`)
//!   and Codex (`model_auto_compact_token_limit` in `~/.codex/config.toml`).
//!   A value th didn't write is the user's and is never touched; what th wrote
//!   is recorded in `~/.smooth/harness/compact-owned.json`.
//!
//! The pearl handoff rides along unchanged: PreCompact checkpoints it and the
//! `SessionStart` `compact` hook re-injects it (`handoff-context.sh`).
//! `harness.context_reset = clear` keeps the old `/th-clear` flow instead.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde_json::Value;
use smooth_policy::settings::Resolver;

/// Claude Code accepts an auto-compact window in this range.
pub const CLAUDE_MIN_WINDOW: u64 = 100_000;
pub const CLAUDE_MAX_WINDOW: u64 = 1_000_000;
/// The Claude Code settings key th owns.
pub const CLAUDE_KEY: &str = "autoCompactWindow";
/// The Codex `config.toml` key th owns.
pub const CODEX_KEY: &str = "model_auto_compact_token_limit";

/// How a session resets at its budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reset {
    /// The harness auto-compacts at the budget; the handoff is re-injected.
    Compact,
    /// The budget hook makes the agent run `/th-clear`.
    Clear,
}

impl Reset {
    #[must_use]
    pub fn parse(s: &str) -> Self {
        if s.trim().eq_ignore_ascii_case("clear") {
            Self::Clear
        } else {
            Self::Compact
        }
    }
}

/// One `harness.context_budgets` entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Override {
    /// `None` = any harness (`*`).
    pub harness: Option<String>,
    /// Model glob; `None` = any model.
    pub model: Option<String>,
    pub tokens: u64,
}

impl Override {
    /// Parse `selector=tokens`: `codex`, `claude-code/claude-opus-*`, `*/claude-sonnet-*`.
    ///
    /// # Errors
    /// Missing `=`, an empty selector, or a non-numeric token count.
    pub fn parse(entry: &str) -> Result<Self> {
        let Some((sel, n)) = entry.split_once('=') else {
            bail!("`{entry}`: expected selector=tokens");
        };
        let tokens: u64 = n
            .trim()
            .replace('_', "")
            .parse()
            .with_context(|| format!("`{entry}`: tokens must be a whole number"))?;
        let sel = sel.trim();
        if sel.is_empty() {
            bail!("`{entry}`: empty selector");
        }
        let (h, m) = sel.split_once('/').map_or((sel, None), |(h, m)| (h, Some(m.trim())));
        let harness = match h.trim() {
            "" | "*" => None,
            h => Some(canonical_harness(h).to_string()),
        };
        let model = m.filter(|m| !m.is_empty() && *m != "*").map(str::to_string);
        Ok(Self { harness, model, tokens })
    }

    /// Does this entry apply, and how specific is it? Higher wins:
    /// harness+model (3) > model only (2) > harness only (1) > `*` (0).
    fn rank(&self, harness: &str, model: Option<&str>) -> Option<(u8, usize)> {
        if self.harness.as_deref().is_some_and(|h| h != canonical_harness(harness)) {
            return None;
        }
        match (&self.model, model) {
            (Some(p), Some(m)) if glob_match(p, m) => {}
            (Some(_), _) => return None,
            (None, _) => {}
        }
        let rank = u8::from(self.harness.is_some()) + 2 * u8::from(self.model.is_some());
        Some((rank, self.model.as_deref().map_or(0, str::len)))
    }
}

/// `claude` → `claude-code`; everything else as given.
#[must_use]
pub fn canonical_harness(h: &str) -> &str {
    match h {
        "claude" => "claude-code",
        other => other,
    }
}

/// `*` matches any run of characters; everything else literally.
#[must_use]
pub fn glob_match(pattern: &str, s: &str) -> bool {
    let (p, s) = (pattern.as_bytes(), s.as_bytes());
    let (mut pi, mut si, mut star, mut mark) = (0, 0, None, 0);
    while si < s.len() {
        if pi < p.len() && p[pi] != b'*' && p[pi] == s[si] {
            pi += 1;
            si += 1;
        } else if pi < p.len() && p[pi] == b'*' {
            star = Some(pi);
            pi += 1;
            mark = si;
        } else if let Some(st) = star {
            pi = st + 1;
            mark += 1;
            si = mark;
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|&c| c == b'*')
}

/// The resolved budget settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Budgets {
    pub global: u64,
    pub warn: u64,
    pub overrides: Vec<Override>,
    pub reset: Reset,
    /// Context past which a milestone suggests `/th-clear` (0 = never).
    pub suggest: u64,
    pub suggest_overrides: Vec<Override>,
}

/// The most specific entry of `overrides` for this session, if any.
fn best_override(overrides: &[Override], harness: &str, model: Option<&str>) -> Option<u64> {
    overrides
        .iter()
        .filter_map(|o| o.rank(harness, model).map(|r| (r, o.tokens)))
        // max_by_key keeps the LAST of equal keys: a later entry wins a tie.
        .max_by_key(|(r, _)| *r)
        .map(|(_, t)| t)
}

impl Budgets {
    #[must_use]
    pub fn from_settings() -> Self {
        Self::from_resolver(&Resolver::from_process())
    }

    /// Unset / unparseable ints are 0 (disabled); bad override entries are skipped.
    #[must_use]
    pub fn from_resolver(r: &Resolver) -> Self {
        let raw = |k: &str| r.resolve(k).ok().and_then(|v| v.value);
        let int = |k: &str| raw(k).and_then(|v| v.trim().parse::<i64>().ok()).map_or(0, |n| u64::try_from(n).unwrap_or(0));
        let list = |k: &str| {
            raw(k)
                .map(|v| {
                    v.split(|c: char| c == ',' || c.is_whitespace())
                        .filter(|e| !e.is_empty())
                        .filter_map(|e| Override::parse(e).ok())
                        .collect()
                })
                .unwrap_or_default()
        };
        Self {
            global: int("harness.context_budget"),
            warn: int("harness.context_budget_warn"),
            overrides: list("harness.context_budgets"),
            reset: Reset::parse(&raw("harness.context_reset").unwrap_or_default()),
            suggest: int("harness.context_suggest"),
            suggest_overrides: list("harness.context_suggests"),
        }
    }

    /// `(budget, warn)` for a session. An override keeps the global gap
    /// between warn and budget (falling back to 90% when the gap is too big);
    /// a warn of 0 stays off.
    #[must_use]
    pub fn for_session(&self, harness: &str, model: Option<&str>) -> (u64, u64) {
        let Some(budget) = best_override(&self.overrides, harness, model) else {
            return (self.global, self.warn);
        };
        if budget == 0 || self.warn == 0 {
            return (budget, if budget == 0 { 0 } else { self.warn.min(budget) });
        }
        let gap = self.global.saturating_sub(self.warn);
        let warn = if gap > 0 && gap < budget { budget - gap } else { budget / 10 * 9 };
        (budget, warn)
    }

    /// Context past which a milestone suggests `/th-clear` for this session
    /// (0 = never): a `harness.context_suggests` entry, else the global line.
    #[must_use]
    pub fn suggest_for(&self, harness: &str, model: Option<&str>) -> u64 {
        best_override(&self.suggest_overrides, harness, model).unwrap_or(self.suggest)
    }

    /// The threshold to write into `harness`'s own config, or `None` to leave
    /// none (budget off, or `/th-clear` mode).
    #[must_use]
    pub fn native_threshold(&self, harness: &str) -> Option<u64> {
        if self.reset == Reset::Clear {
            return None;
        }
        let (budget, _) = self.for_session(harness, None);
        (budget > 0).then_some(budget)
    }
}

// ── native thresholds ──────────────────────────────────────────────

/// What a sync did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Sync {
    /// Wrote (or replaced th's earlier) value.
    Set(u64),
    /// Already at the wanted value.
    Unchanged(u64),
    /// Removed th's value (budget off or `/th-clear` mode).
    Removed,
    /// Nothing there and nothing wanted.
    Off,
    /// The user set their own value; left alone.
    UserOwned(u64),
}

fn owned_path(home: &Path) -> PathBuf {
    home.join(".smooth").join("harness").join("compact-owned.json")
}

fn load_owned(home: &Path) -> BTreeMap<String, u64> {
    fs::read(owned_path(home))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn save_owned(home: &Path, owned: &BTreeMap<String, u64>) -> Result<()> {
    let p = owned_path(home);
    if let Some(dir) = p.parent() {
        fs::create_dir_all(dir)?;
    }
    fs::write(&p, serde_json::to_vec_pretty(owned)?).with_context(|| format!("write {}", p.display()))
}

/// What a sync does to the harness config.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edit {
    Keep,
    Write(u64),
    Remove,
}

/// The pure ownership rule: `current` in the harness config, `owned` = what
/// th last wrote, `want` = what th wants now.
#[must_use]
pub fn plan(current: Option<u64>, owned: Option<u64>, want: Option<u64>) -> (Sync, Edit) {
    match (current, want) {
        (Some(c), _) if owned != Some(c) => (Sync::UserOwned(c), Edit::Keep),
        (Some(c), Some(w)) if c == w => (Sync::Unchanged(w), Edit::Keep),
        (_, Some(w)) => (Sync::Set(w), Edit::Write(w)),
        (Some(_), None) => (Sync::Removed, Edit::Remove),
        (None, None) => (Sync::Off, Edit::Keep),
    }
}

fn record(home: &Path, harness: &str, outcome: &Sync) -> Result<()> {
    let mut owned = load_owned(home);
    let changed = match outcome {
        Sync::Set(v) | Sync::Unchanged(v) => owned.insert(harness.to_string(), *v) != Some(*v),
        Sync::Removed => owned.remove(harness).is_some(),
        Sync::Off | Sync::UserOwned(_) => false,
    };
    if changed {
        save_owned(home, &owned)?;
    }
    Ok(())
}

/// Clamp a budget into the range Claude Code accepts.
#[must_use]
pub const fn claude_window(budget: u64) -> u64 {
    if budget < CLAUDE_MIN_WINDOW {
        CLAUDE_MIN_WINDOW
    } else if budget > CLAUDE_MAX_WINDOW {
        CLAUDE_MAX_WINDOW
    } else {
        budget
    }
}

fn claude_settings(home: &Path) -> PathBuf {
    home.join(".claude").join("settings.json")
}

/// Set (or clear) `autoCompactWindow` in `~/.claude/settings.json`. Keys and
/// order the user has are preserved; a value th didn't write is left alone.
///
/// # Errors
/// An unreadable / non-object settings file (never clobbered) or a failed write.
pub fn sync_claude(home: &Path, want: Option<u64>) -> Result<Sync> {
    let path = claude_settings(home);
    let mut doc: Value = if path.exists() {
        let raw = fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        if raw.trim().is_empty() {
            Value::Object(serde_json::Map::new())
        } else {
            serde_json::from_str(&raw).with_context(|| format!("parse {} — fix it, then re-run", path.display()))?
        }
    } else {
        Value::Object(serde_json::Map::new())
    };
    let Some(obj) = doc.as_object_mut() else {
        bail!("{} is not a JSON object", path.display());
    };
    let current = obj.get(CLAUDE_KEY).and_then(Value::as_u64);
    let (outcome, edit) = plan(current, load_owned(home).get("claude-code").copied(), want.map(claude_window));
    if edit != Edit::Keep {
        if let Edit::Write(v) = edit {
            obj.insert(CLAUDE_KEY.to_string(), Value::from(v));
        } else {
            obj.remove(CLAUDE_KEY);
        }
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        fs::write(&path, format!("{}\n", serde_json::to_string_pretty(&doc)?)).with_context(|| format!("write {}", path.display()))?;
    }
    record(home, "claude-code", &outcome)?;
    Ok(outcome)
}

fn codex_config(home: &Path) -> PathBuf {
    home.join(".codex").join("config.toml")
}

/// Set (or clear) the top-level `model_auto_compact_token_limit` in
/// `~/.codex/config.toml`, in place (comments and layout survive).
///
/// # Errors
/// An unparseable config (never clobbered) or a failed write.
pub fn sync_codex(home: &Path, want: Option<u64>) -> Result<Sync> {
    let path = codex_config(home);
    let raw = if path.exists() { fs::read_to_string(&path)? } else { String::new() };
    let mut doc: toml_edit::DocumentMut = raw.parse().with_context(|| format!("parse {} — fix it, then re-run", path.display()))?;
    let current = doc.get(CODEX_KEY).and_then(toml_edit::Item::as_integer).and_then(|n| u64::try_from(n).ok());
    let (outcome, edit) = plan(current, load_owned(home).get("codex").copied(), want);
    if edit != Edit::Keep {
        if let Edit::Write(v) = edit {
            let fresh = current.is_none();
            doc[CODEX_KEY] = toml_edit::value(i64::try_from(v).unwrap_or(i64::MAX));
            if fresh {
                if let Some(mut key) = doc.as_table_mut().key_mut(CODEX_KEY) {
                    key.leaf_decor_mut().set_prefix(
                        "# Auto-compact at harness.context_budget; written by `th harness enable codex` (th settings set harness.context_budget …).\n",
                    );
                }
            }
        } else {
            doc.remove(CODEX_KEY);
        }
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        fs::write(&path, doc.to_string()).with_context(|| format!("write {}", path.display()))?;
    }
    record(home, "codex", &outcome)?;
    Ok(outcome)
}

/// One status line for a sync outcome.
#[must_use]
pub fn describe(outcome: &Sync, where_: &str) -> String {
    let k = |n: u64| format!("{}k", n / 1000);
    match outcome {
        Sync::Set(v) => format!("auto-compacts at {} ({where_}) — new sessions pick it up", k(*v)),
        Sync::Unchanged(v) => format!("auto-compacts at {} ({where_})", k(*v)),
        Sync::Removed => format!("th's auto-compact threshold removed from {where_} (budget off or harness.context_reset=clear)"),
        Sync::Off => "auto-compact: harness default (budget off or harness.context_reset=clear)".to_string(),
        Sync::UserOwned(v) => format!(
            "auto-compacts at {} — your own {where_} value, left alone (remove it to let th manage it)",
            k(*v)
        ),
    }
}

/// The current native threshold, read-only (for `th harness status`).
#[must_use]
pub fn current(home: &Path, harness: &str) -> Option<u64> {
    match harness {
        "claude-code" => fs::read_to_string(claude_settings(home))
            .ok()
            .and_then(|r| serde_json::from_str::<Value>(&r).ok())
            .and_then(|v| v.get(CLAUDE_KEY).and_then(Value::as_u64)),
        "codex" => fs::read_to_string(codex_config(home))
            .ok()
            .and_then(|r| r.parse::<toml_edit::DocumentMut>().ok())
            .and_then(|d| d.get(CODEX_KEY).and_then(toml_edit::Item::as_integer))
            .and_then(|n| u64::try_from(n).ok()),
        _ => None,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, reason = "unwrap/expect are the idiom for test assertions")]
mod tests {
    use super::*;
    use smooth_policy::settings::SettingsFile;
    use tempfile::TempDir;

    fn budgets(global: u64, warn: u64, overrides: &[&str]) -> Budgets {
        Budgets {
            global,
            warn,
            overrides: overrides.iter().map(|e| Override::parse(e).unwrap()).collect(),
            reset: Reset::Compact,
            suggest: 150_000,
            suggest_overrides: Vec::new(),
        }
    }

    #[test]
    fn reset_parses_with_compact_as_the_default() {
        assert_eq!(Reset::parse("clear"), Reset::Clear);
        assert_eq!(Reset::parse(" CLEAR "), Reset::Clear);
        assert_eq!(Reset::parse("compact"), Reset::Compact);
        assert_eq!(Reset::parse(""), Reset::Compact);
        assert_eq!(Reset::parse("junk"), Reset::Compact);
    }

    #[test]
    fn override_parses_every_selector_shape() {
        assert_eq!(
            Override::parse("codex=200000").unwrap(),
            Override {
                harness: Some("codex".into()),
                model: None,
                tokens: 200_000
            }
        );
        assert_eq!(
            Override::parse("claude/claude-opus-*=300_000").unwrap(),
            Override {
                harness: Some("claude-code".into()),
                model: Some("claude-opus-*".into()),
                tokens: 300_000
            }
        );
        assert_eq!(
            Override::parse("*/claude-sonnet-*=180000").unwrap(),
            Override {
                harness: None,
                model: Some("claude-sonnet-*".into()),
                tokens: 180_000
            }
        );
        assert_eq!(Override::parse("*=1").unwrap().harness, None);
        assert_eq!(Override::parse("codex/*=5").unwrap().model, None);
    }

    #[test]
    fn override_rejects_junk() {
        assert!(Override::parse("codex").is_err());
        assert!(Override::parse("=5").is_err());
        assert!(Override::parse("codex=lots").is_err());
        assert!(Override::parse("codex=-5").is_err());
    }

    #[test]
    fn glob_matches_stars_anywhere() {
        assert!(glob_match("claude-opus-*", "claude-opus-5-5"));
        assert!(glob_match("*sonnet*", "claude-sonnet-5-5"));
        assert!(glob_match("*", ""));
        assert!(glob_match("claude-opus-5-5", "claude-opus-5-5"));
        assert!(!glob_match("claude-opus-5-5", "claude-opus-5-5x"));
        assert!(!glob_match("claude-opus-*", "claude-sonnet-5"));
        assert!(glob_match("a*b*c", "axxbyyc"));
        assert!(!glob_match("a*b*c", "axxbyy"));
    }

    #[test]
    fn no_overrides_means_the_global_budget_everywhere() {
        let b = budgets(250_000, 220_000, &[]);
        assert_eq!(b.for_session("claude-code", Some("claude-opus-5-5")), (250_000, 220_000));
        assert_eq!(b.for_session("codex", None), (250_000, 220_000));
    }

    #[test]
    fn the_most_specific_override_wins() {
        let b = budgets(
            250_000,
            220_000,
            &[
                "claude-code=200000",
                "*/claude-opus-*=400000",
                "claude-code/claude-opus-5*=300000",
                "codex=150000",
            ],
        );
        assert_eq!(b.for_session("claude-code", Some("claude-opus-5-5")).0, 300_000, "harness+model");
        assert_eq!(b.for_session("claude-code", Some("claude-opus-4-8")).0, 400_000, "model beats harness");
        assert_eq!(b.for_session("claude-code", Some("claude-sonnet-5-5")).0, 200_000, "harness");
        assert_eq!(b.for_session("claude-code", None).0, 200_000, "model-specific entries need a model");
        assert_eq!(b.for_session("opencode", Some("claude-opus-5")).0, 400_000);
        assert_eq!(b.for_session("opencode", Some("gpt-6")).0, 250_000, "no match → global");
        assert_eq!(b.for_session("claude", None).0, 200_000, "claude is claude-code");
    }

    #[test]
    fn a_later_equal_entry_wins_and_a_longer_glob_beats_a_shorter_one() {
        let b = budgets(250_000, 220_000, &["codex=100000", "codex=120000"]);
        assert_eq!(b.for_session("codex", None).0, 120_000);
        let b = budgets(250_000, 220_000, &["*/claude-*=100000", "*/claude-opus-*=300000"]);
        assert_eq!(b.for_session("x", Some("claude-opus-5")).0, 300_000);
    }

    #[test]
    fn an_override_keeps_the_warn_gap() {
        let b = budgets(250_000, 220_000, &["codex=150000", "cursor=20000"]);
        assert_eq!(b.for_session("codex", None), (150_000, 120_000));
        assert_eq!(b.for_session("cursor", None), (20_000, 18_000), "gap too big → 90%");
        let off = budgets(250_000, 0, &["codex=150000"]);
        assert_eq!(off.for_session("codex", None), (150_000, 0), "warn off stays off");
        let zero = budgets(250_000, 220_000, &["codex=0"]);
        assert_eq!(zero.for_session("codex", None), (0, 0), "an override of 0 disables the budget");
    }

    #[test]
    fn suggest_takes_the_most_specific_override() {
        let mut b = budgets(300_000, 270_000, &[]);
        b.suggest_overrides = vec![Override::parse("*/claude-sonnet-*=120000").unwrap(), Override::parse("codex=0").unwrap()];
        assert_eq!(b.suggest_for("claude-code", Some("claude-sonnet-5-5")), 120_000);
        assert_eq!(b.suggest_for("claude-code", Some("claude-opus-5-5")), 150_000);
        assert_eq!(b.suggest_for("claude-code", None), 150_000);
        assert_eq!(b.suggest_for("codex", None), 0, "an override of 0 turns suggestions off");
    }

    #[test]
    fn native_threshold_is_off_in_clear_mode_and_when_disabled() {
        let mut b = budgets(250_000, 220_000, &["codex=0"]);
        assert_eq!(b.native_threshold("claude-code"), Some(250_000));
        assert_eq!(b.native_threshold("codex"), None);
        b.reset = Reset::Clear;
        assert_eq!(b.native_threshold("claude-code"), None);
    }

    #[test]
    fn from_resolver_reads_every_key() {
        let file = SettingsFile::parse(
            "[harness]\ncontext_budget = 300000\ncontext_budget_warn = 260000\ncontext_reset = \"clear\"\ncontext_budgets = [\"codex=150000\", \"bogus\"]\ncontext_suggest = 100000\ncontext_suggests = [\"*/claude-sonnet-*=120000\"]\n",
        )
        .unwrap();
        let r = Resolver::new(|_| None, file);
        let b = Budgets::from_resolver(&r);
        assert_eq!(b.global, 300_000);
        assert_eq!(b.warn, 260_000);
        assert_eq!(b.reset, Reset::Clear);
        assert_eq!(b.overrides.len(), 1, "a bad entry is skipped, not fatal");
        assert_eq!(b.suggest_for("claude-code", Some("claude-sonnet-5-5")), 120_000);
        assert_eq!(b.suggest_for("claude-code", Some("claude-opus-5-5")), 100_000);
        let d = Budgets::from_resolver(&Resolver::new(|_| None, SettingsFile::empty()));
        assert_eq!((d.global, d.warn, d.suggest, d.reset), (300_000, 270_000, 150_000, Reset::Compact));
    }

    #[test]
    fn plan_follows_the_ownership_rule() {
        assert_eq!(plan(None, None, Some(250_000)), (Sync::Set(250_000), Edit::Write(250_000)));
        assert_eq!(plan(Some(250_000), Some(250_000), Some(250_000)), (Sync::Unchanged(250_000), Edit::Keep));
        assert_eq!(plan(Some(250_000), Some(250_000), Some(300_000)), (Sync::Set(300_000), Edit::Write(300_000)));
        assert_eq!(plan(Some(250_000), Some(250_000), None), (Sync::Removed, Edit::Remove));
        assert_eq!(plan(Some(400_000), None, Some(250_000)), (Sync::UserOwned(400_000), Edit::Keep), "user's value");
        assert_eq!(
            plan(Some(400_000), Some(250_000), None),
            (Sync::UserOwned(400_000), Edit::Keep),
            "user changed ours"
        );
        assert_eq!(plan(None, Some(250_000), None), (Sync::Off, Edit::Keep));
        assert_eq!(plan(None, None, None), (Sync::Off, Edit::Keep));
    }

    #[test]
    fn claude_window_clamps_to_the_accepted_range() {
        assert_eq!(claude_window(50_000), CLAUDE_MIN_WINDOW);
        assert_eq!(claude_window(250_000), 250_000);
        assert_eq!(claude_window(2_000_000), CLAUDE_MAX_WINDOW);
    }

    #[test]
    fn sync_claude_sets_keeps_other_keys_and_removes_only_its_own() {
        let home = TempDir::new().unwrap();
        let p = home.path().join(".claude/settings.json");
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, r#"{"model":"opus","env":{"A":"1"}}"#).unwrap();

        assert_eq!(sync_claude(home.path(), Some(250_000)).unwrap(), Sync::Set(250_000));
        let v: Value = serde_json::from_str(&fs::read_to_string(&p).unwrap()).unwrap();
        assert_eq!(v["autoCompactWindow"], 250_000);
        assert_eq!(v["model"], "opus");
        assert_eq!(v["env"]["A"], "1");
        let keys: Vec<&String> = v.as_object().unwrap().keys().collect();
        assert_eq!(keys, ["model", "env", "autoCompactWindow"], "order preserved");

        assert_eq!(sync_claude(home.path(), Some(250_000)).unwrap(), Sync::Unchanged(250_000));
        assert_eq!(sync_claude(home.path(), Some(40_000)).unwrap(), Sync::Set(CLAUDE_MIN_WINDOW), "clamped");
        assert_eq!(current(home.path(), "claude-code"), Some(CLAUDE_MIN_WINDOW));
        assert_eq!(sync_claude(home.path(), None).unwrap(), Sync::Removed);
        let v: Value = serde_json::from_str(&fs::read_to_string(&p).unwrap()).unwrap();
        assert!(v.get("autoCompactWindow").is_none());
        assert_eq!(sync_claude(home.path(), None).unwrap(), Sync::Off);
    }

    #[test]
    fn sync_claude_leaves_a_users_own_value_alone() {
        let home = TempDir::new().unwrap();
        let p = home.path().join(".claude/settings.json");
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, r#"{"autoCompactWindow":500000}"#).unwrap();
        let before = fs::read_to_string(&p).unwrap();
        assert_eq!(sync_claude(home.path(), Some(250_000)).unwrap(), Sync::UserOwned(500_000));
        assert_eq!(sync_claude(home.path(), None).unwrap(), Sync::UserOwned(500_000));
        assert_eq!(fs::read_to_string(&p).unwrap(), before, "untouched");
    }

    #[test]
    fn sync_claude_creates_a_missing_file_and_refuses_a_broken_one() {
        let home = TempDir::new().unwrap();
        assert_eq!(sync_claude(home.path(), Some(300_000)).unwrap(), Sync::Set(300_000));
        assert_eq!(current(home.path(), "claude-code"), Some(300_000));
        fs::write(home.path().join(".claude/settings.json"), "{ nope").unwrap();
        assert!(sync_claude(home.path(), Some(300_000)).is_err());
        fs::write(home.path().join(".claude/settings.json"), "[1]").unwrap();
        assert!(sync_claude(home.path(), Some(300_000)).is_err());
    }

    #[test]
    fn sync_codex_writes_a_commented_top_level_key_in_place() {
        let home = TempDir::new().unwrap();
        let p = home.path().join(".codex/config.toml");
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, "# mine\nmodel = \"gpt-6\"\n\n[sandbox_workspace_write]\nwritable_roots = [\"/x\"]\n").unwrap();

        assert_eq!(sync_codex(home.path(), Some(250_000)).unwrap(), Sync::Set(250_000));
        let raw = fs::read_to_string(&p).unwrap();
        assert!(raw.contains("# mine"), "comments survive: {raw}");
        assert!(raw.contains("# Auto-compact at harness.context_budget"), "{raw}");
        let doc: toml::Value = raw.parse().unwrap();
        assert_eq!(
            doc["model_auto_compact_token_limit"].as_integer(),
            Some(250_000),
            "top level, not in a table: {raw}"
        );
        assert_eq!(doc["sandbox_workspace_write"]["writable_roots"][0].as_str(), Some("/x"));

        assert_eq!(sync_codex(home.path(), Some(250_000)).unwrap(), Sync::Unchanged(250_000));
        assert_eq!(sync_codex(home.path(), Some(2_000_000)).unwrap(), Sync::Set(2_000_000), "codex is not clamped");
        assert_eq!(current(home.path(), "codex"), Some(2_000_000));
        assert_eq!(sync_codex(home.path(), None).unwrap(), Sync::Removed);
        assert!(!fs::read_to_string(&p).unwrap().contains("model_auto_compact_token_limit"));
    }

    #[test]
    fn sync_codex_leaves_a_users_own_value_alone() {
        let home = TempDir::new().unwrap();
        let p = home.path().join(".codex/config.toml");
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, "model_auto_compact_token_limit = 600000\n").unwrap();
        assert_eq!(sync_codex(home.path(), Some(250_000)).unwrap(), Sync::UserOwned(600_000));
        assert_eq!(fs::read_to_string(&p).unwrap(), "model_auto_compact_token_limit = 600000\n");
    }

    #[test]
    fn ownership_is_tracked_per_harness() {
        let home = TempDir::new().unwrap();
        sync_claude(home.path(), Some(250_000)).unwrap();
        sync_codex(home.path(), Some(200_000)).unwrap();
        let owned = load_owned(home.path());
        assert_eq!(owned.get("claude-code"), Some(&250_000));
        assert_eq!(owned.get("codex"), Some(&200_000));
        sync_codex(home.path(), None).unwrap();
        let owned = load_owned(home.path());
        assert_eq!(owned.get("codex"), None);
        assert_eq!(owned.get("claude-code"), Some(&250_000));
    }

    #[test]
    fn describe_names_every_outcome() {
        assert!(describe(&Sync::Set(250_000), "x").contains("250k"));
        assert!(describe(&Sync::Unchanged(250_000), "x").contains("250k"));
        assert!(describe(&Sync::Removed, "x").contains("removed"));
        assert!(describe(&Sync::Off, "x").contains("default"));
        assert!(describe(&Sync::UserOwned(500_000), "x").contains("left alone"));
    }
}

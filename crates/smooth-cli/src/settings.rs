//! `th settings` — one machine settings file for Smooth's knobs (pearl th-f95ecf).
//!
//! The registry, file and resolver live in `smooth_policy::settings` so the
//! daemon and the tools read exactly what this command writes. This module is
//! only the CLI over it: `list` / `show` (alias `get`) / `set` / `unset` /
//! `explain` / `path`, every read verb with `--json` for agents.
//!
//! Named `settings`, not `config`: `th config` is the hidden, load-bearing
//! compat alias for `smoo config` (the Smoo AI platform config server,
//! th-845c06).
//!
//! Precedence is legacy env var > file > default. The env column reflects
//! THIS shell; Big Smooth sees the environment it was launched with.

use anstream::{eprintln, print, println};
use anyhow::{bail, Result};
use clap::Subcommand;
use owo_colors::OwoColorize;
use serde_json::{json, Value};
use smooth_policy::settings::{self, Resolved, Resolver, SettingDef, SettingsFile, Source, REGISTRY, RESTART_HINT};

#[derive(Subcommand, Debug)]
pub enum Cmd {
    /// Every setting: value, where it came from, default, and whether a
    /// change needs a Big Smooth restart.
    #[command(visible_alias = "ls")]
    List {
        #[arg(long)]
        json: bool,
    },
    /// One setting's effective value (stdout is just the value).
    #[command(visible_alias = "get")]
    Show {
        /// Dotted key, e.g. `sandbox.enabled`.
        key: String,
        #[arg(long)]
        json: bool,
    },
    /// Write a setting to the settings file (validated against its type).
    ///
    /// Bools take true/false (or 1/0, yes/no, on/off); lists take
    /// comma-separated entries. Prints how to apply the change.
    Set {
        /// Dotted key, e.g. `sandbox.enabled`.
        key: String,
        /// The new value.
        value: String,
        #[arg(long)]
        json: bool,
    },
    /// Remove a setting from the file (it falls back to its default).
    Unset {
        /// Dotted key, e.g. `sandbox.enabled`.
        key: String,
        #[arg(long)]
        json: bool,
    },
    /// Everything about one setting: what it does, type, allowed values,
    /// default, env var, who reads it, how to apply a change.
    Explain {
        /// Dotted key, e.g. `sandbox.enabled`.
        key: String,
        #[arg(long)]
        json: bool,
    },
    /// Print the settings file path.
    Path {
        #[arg(long)]
        json: bool,
    },
}

/// Run a `th settings` subcommand.
///
/// # Errors
/// Unknown key (with a suggestion), a value that doesn't fit the key's type,
/// a malformed settings file on write, or an I/O failure.
pub fn cmd(cmd: Cmd) -> Result<()> {
    match cmd {
        Cmd::List { json } => {
            let resolver = Resolver::from_process();
            if json {
                print_json(&list_json(&resolver));
            } else {
                print!("{}", list_text(&resolver));
                warn_file_problems(&resolver);
            }
            Ok(())
        }
        Cmd::Show { key, json } => {
            let resolver = Resolver::from_process();
            let r = resolver.resolve(&key)?;
            if json {
                print_json(&r.to_json());
            } else {
                match &r.value {
                    Some(v) => println!("{v}"),
                    None => eprintln!("{}", format!("unset — {}", r.def.default_note).dimmed()),
                }
                eprintln!("{}", format!("source: {}{}", r.source, source_detail(&r)).dimmed());
            }
            warn_file_problems_for(&resolver, &r, json);
            Ok(())
        }
        Cmd::Explain { key, json } => {
            let resolver = Resolver::from_process();
            let r = resolver.resolve(&key)?;
            if json {
                let mut j = r.to_json();
                j["path"] = path_json();
                j["restart_hint"] = restart_hint_json(r.def);
                print_json(&j);
            } else {
                print!("{}", explain_text(&r));
            }
            Ok(())
        }
        Cmd::Set { key, value, json } => {
            let def = settings::require_def(&key)?;
            let typed = settings::parse_value(def, &value)?;
            let mut file = SettingsFile::load_default()?;
            if file.path().is_none() {
                bail!("can't locate the settings file (HOME is unset)\nset HOME or SMOOTH_HOME and retry");
            }
            file.set(def, &typed)?;
            file.save()?;
            let env_value = std::env::var(def.env).ok();
            let out = change_json(def, Some(typed.to_json()), file.path(), true, env_value.as_deref());
            if json {
                print_json(&out);
            } else {
                println!(
                    "{} {} = {}  {}",
                    "✓".green().bold(),
                    def.key.bold(),
                    typed.to_raw(),
                    format!("({})", display_path(file.path())).dimmed()
                );
                print_apply_notes(def, env_value.as_deref());
            }
            Ok(())
        }
        Cmd::Unset { key, json } => {
            let def = settings::require_def(&key)?;
            let mut file = SettingsFile::load_default()?;
            let removed = file.unset(def);
            if removed {
                file.save()?;
            }
            let env_value = std::env::var(def.env).ok();
            if json {
                print_json(&change_json(def, None, file.path(), removed, env_value.as_deref()));
            } else if removed {
                println!(
                    "{} {} unset — back to the default ({})",
                    "✓".green().bold(),
                    def.key.bold(),
                    def.default.unwrap_or(def.default_note)
                );
                print_apply_notes(def, env_value.as_deref());
            } else {
                println!("{} was not set in {} — nothing to do", def.key.bold(), display_path(file.path()));
            }
            Ok(())
        }
        Cmd::Path { json } => {
            if json {
                print_json(&json!({ "path": path_json(), "exists": settings::settings_path().is_some_and(|p| p.exists()) }));
            } else {
                println!("{}", display_path(settings::settings_path().as_deref()));
            }
            Ok(())
        }
    }
}

fn print_json(v: &Value) {
    println!("{}", serde_json::to_string_pretty(v).unwrap_or_default());
}

fn path_json() -> Value {
    settings::settings_path().map_or(Value::Null, |p| Value::String(p.display().to_string()))
}

fn display_path(p: Option<&std::path::Path>) -> String {
    p.map_or_else(|| "(no settings path: HOME is unset)".to_owned(), |p| p.display().to_string())
}

fn restart_hint_json(def: &SettingDef) -> Value {
    if def.apply.restart_required() {
        Value::String(RESTART_HINT.to_owned())
    } else {
        Value::Null
    }
}

/// The stable `--json` for `th settings list`.
pub(crate) fn list_json(resolver: &Resolver) -> Value {
    json!({
        "path": resolver.file().path().map(|p| p.display().to_string()),
        "exists": resolver.file().exists(),
        "file_error": resolver.file_load_error(),
        "settings": resolver.resolve_all().iter().map(Resolved::to_json).collect::<Vec<_>>(),
        "unknown_keys": resolver.file().unknown_keys(),
    })
}

/// The stable `--json` for `set` / `unset`.
fn change_json(def: &SettingDef, value: Option<Value>, path: Option<&std::path::Path>, changed: bool, env_value: Option<&str>) -> Value {
    json!({
        "key": def.key,
        "value": value,
        "changed": changed,
        "path": path.map(|p| p.display().to_string()),
        "restart_required": def.apply.restart_required(),
        "restart_hint": restart_hint_json(def),
        "env_override": env_value.map(|v| json!({ "env": def.env, "value": v })),
    })
}

fn source_detail(r: &Resolved) -> String {
    match r.source {
        Source::Env => format!(" ({}={})", r.def.env, r.env_value.as_deref().unwrap_or("")),
        Source::File | Source::Default => String::new(),
    }
}

/// Render the value column: the effective value, or `(unset)`.
fn value_cell(r: &Resolved) -> String {
    r.value
        .clone()
        .map_or_else(|| "(unset)".to_owned(), |v| if v.is_empty() { "\"\"".to_owned() } else { v })
}

fn list_text(resolver: &Resolver) -> String {
    use std::fmt::Write as _;
    let rows = resolver.resolve_all();
    let kw = rows.iter().map(|r| r.def.key.len()).max().unwrap_or(0);
    let vw = rows.iter().map(|r| value_cell(r).chars().count()).max().unwrap_or(0).min(40);
    let mut out = String::new();
    let _ = writeln!(
        out,
        "{} {}",
        "Settings".bold(),
        format!(
            "{}{}",
            display_path(resolver.file().path()),
            if resolver.file().exists() { "" } else { " (not created yet)" }
        )
        .dimmed()
    );
    for r in &rows {
        let source = match r.source {
            Source::Env => format!("● env {}", r.def.env).yellow().to_string(),
            Source::File => "● file".cyan().to_string(),
            Source::Default => "○ default".dimmed().to_string(),
        };
        let restart = if r.def.apply.restart_required() {
            "  ↻ restart".dimmed().to_string()
        } else {
            String::new()
        };
        let _ = writeln!(out, "  {:kw$}  {:vw$}  {source}{restart}", r.def.key.bold(), value_cell(r));
        let _ = writeln!(out, "  {:kw$}  {}", "", r.def.description.dimmed());
    }
    let _ = writeln!(
        out,
        "{}",
        "Precedence: env var > file > default. `env` reflects this shell — Big Smooth sees the environment it was launched with.\n\
         ↻ = restart Big Smooth to apply a change. `th settings explain <key>` for details."
            .dimmed()
    );
    out
}

fn explain_text(r: &Resolved) -> String {
    use std::fmt::Write as _;
    let d = r.def;
    let mut out = String::new();
    let _ = writeln!(out, "{}", d.key.bold());
    let _ = writeln!(out, "  {}", d.description);
    let _ = writeln!(out);
    let _ = writeln!(out, "  type        {}", d.kind.name());
    if !d.kind.allowed().is_empty() {
        let _ = writeln!(out, "  allowed     {}", d.kind.allowed().join(", "));
    }
    let _ = writeln!(out, "  default     {}", d.default.unwrap_or(d.default_note));
    let _ = writeln!(out, "  value       {}  ({}{})", value_cell(r), r.source, source_detail(r));
    if let Some(e) = &r.file_error {
        let _ = writeln!(out, "  file        ignored: {e}");
    }
    let _ = writeln!(out, "  env var     {}  (overrides the file)", d.env);
    let _ = writeln!(out, "  read by     {}", d.component);
    let _ = writeln!(
        out,
        "  applies     {}",
        if d.apply.restart_required() {
            format!("after a Big Smooth restart: {RESTART_HINT}")
        } else {
            "immediately".to_owned()
        }
    );
    let example = d.default.map_or("<value>", |v| match v {
        "true" => "false",
        "false" => "true",
        other => other,
    });
    let _ = writeln!(out);
    let _ = writeln!(out, "  th settings set {} {example}", d.key);
    let _ = writeln!(out, "  th settings unset {}", d.key);
    out
}

fn print_apply_notes(def: &SettingDef, env_value: Option<&str>) {
    if def.apply.restart_required() {
        println!("  restart Big Smooth to apply: {RESTART_HINT}");
    }
    if let Some(v) = env_value {
        eprintln!(
            "  {} {}={v:?} is set in this shell and overrides the file here; if Big Smooth was launched with it, unset it there too.",
            "!".yellow().bold(),
            def.env
        );
    }
}

fn warn_file_problems(resolver: &Resolver) {
    if let Some(e) = resolver.file_load_error() {
        eprintln!("{} settings file ignored: {e}", "!".yellow().bold());
    }
    for r in resolver.resolve_all() {
        if let Some(e) = &r.file_error {
            eprintln!("{} {}: file value ignored: {e}", "!".yellow().bold(), r.def.key);
        }
    }
    let unknown = resolver.file().unknown_keys();
    if !unknown.is_empty() {
        let hints: Vec<String> = unknown
            .iter()
            .map(|k| settings::suggest(k).map_or_else(|| k.clone(), |s| format!("{k} (did you mean {s}?)")))
            .collect();
        eprintln!(
            "{} unknown keys in the settings file (kept, never read): {}",
            "!".yellow().bold(),
            hints.join(", ")
        );
    }
}

fn warn_file_problems_for(resolver: &Resolver, r: &Resolved, json: bool) {
    if json {
        return;
    }
    if let Some(e) = resolver.file_load_error() {
        eprintln!("{} settings file ignored: {e}", "!".yellow().bold());
    }
    if let Some(e) = &r.file_error {
        eprintln!("{} file value ignored: {e}", "!".yellow().bold());
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, reason = "unwrap/expect are the idiom for test assertions")]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Parser, Debug)]
    struct T {
        #[command(subcommand)]
        cmd: Cmd,
    }

    fn parse(args: &[&str]) -> Cmd {
        T::try_parse_from(std::iter::once("settings").chain(args.iter().copied())).expect("parses").cmd
    }

    fn resolver(env: &'static [(&'static str, &'static str)], file: &str) -> Resolver {
        Resolver::new(
            move |name| env.iter().find(|(k, _)| *k == name).map(|(_, v)| (*v).to_owned()),
            SettingsFile::parse(file).unwrap(),
        )
    }

    #[test]
    fn every_verb_parses() {
        assert!(matches!(parse(&["list"]), Cmd::List { json: false }));
        assert!(matches!(parse(&["ls", "--json"]), Cmd::List { json: true }));
        assert!(matches!(parse(&["show", "sandbox.enabled"]), Cmd::Show { ref key, json: false } if key == "sandbox.enabled"));
        assert!(matches!(parse(&["get", "model", "--json"]), Cmd::Show { ref key, json: true } if key == "model"));
        assert!(
            matches!(parse(&["set", "sandbox.enabled", "true"]), Cmd::Set { ref key, ref value, json: false } if key == "sandbox.enabled" && value == "true")
        );
        assert!(matches!(
            parse(&["set", "egress.allowlist", "defaults,github.com", "--json"]),
            Cmd::Set { json: true, .. }
        ));
        assert!(matches!(parse(&["unset", "auto_mode"]), Cmd::Unset { ref key, json: false } if key == "auto_mode"));
        assert!(matches!(parse(&["explain", "relay.url", "--json"]), Cmd::Explain { json: true, .. }));
        assert!(matches!(parse(&["path"]), Cmd::Path { json: false }));
        assert!(T::try_parse_from(["settings", "set", "sandbox.enabled"]).is_err(), "set needs a value");
        assert!(T::try_parse_from(["settings", "show"]).is_err(), "show needs a key");
    }

    #[test]
    fn list_json_shape_is_stable() {
        let r = resolver(&[("SMOOTH_SANDBOX", "1")], "fast_mode = true\n[mine]\nx = 1\n");
        let j = list_json(&r);
        let keys: Vec<&str> = {
            let mut k: Vec<&str> = j.as_object().unwrap().keys().map(String::as_str).collect();
            k.sort_unstable();
            k
        };
        assert_eq!(keys, ["exists", "file_error", "path", "settings", "unknown_keys"]);
        let rows = j["settings"].as_array().unwrap();
        assert_eq!(rows.len(), REGISTRY.len());
        let sandbox = rows.iter().find(|r| r["key"] == "sandbox.enabled").unwrap();
        assert_eq!((sandbox["value"].clone(), sandbox["source"].clone()), (json!(true), json!("env")));
        let fast = rows.iter().find(|r| r["key"] == "fast_mode").unwrap();
        assert_eq!((fast["value"].clone(), fast["source"].clone()), (json!(true), json!("file")));
        assert_eq!(j["unknown_keys"], json!(["mine.x"]));
    }

    #[test]
    fn change_json_reports_restart_and_env_override() {
        let def = settings::def("sandbox.enabled").unwrap();
        let j = change_json(def, Some(json!(true)), Some(std::path::Path::new("/h/.smooth/settings.toml")), true, Some("0"));
        assert_eq!(j["restart_required"], true);
        assert_eq!(j["restart_hint"], RESTART_HINT);
        assert_eq!(j["env_override"], json!({ "env": "SMOOTH_SANDBOX", "value": "0" }));
        assert_eq!(j["path"], "/h/.smooth/settings.toml");
        let j = change_json(def, None, None, false, None);
        assert_eq!(
            (j["value"].clone(), j["changed"].clone(), j["env_override"].clone()),
            (Value::Null, json!(false), Value::Null)
        );
    }

    #[test]
    fn list_text_shows_every_key_with_source() {
        let text = list_text(&resolver(&[("SMOOTH_AUTO_MODE", "ask")], "[sandbox]\nenabled = true\n"));
        for d in REGISTRY {
            assert!(text.contains(d.key), "{} missing from:\n{text}", d.key);
        }
        assert!(text.contains("env SMOOTH_AUTO_MODE"), "{text}");
        assert!(text.contains("● file"), "{text}");
        assert!(text.contains("○ default"), "{text}");
        assert!(text.contains("(unset)"), "keys with no default say so: {text}");
    }

    #[test]
    fn explain_text_covers_the_contract() {
        let r = resolver(&[], "").resolve("sandbox.enabled").unwrap();
        let text = explain_text(&r);
        for needle in [
            "sandbox.enabled",
            "type        bool",
            "default     false",
            "SMOOTH_SANDBOX",
            "read by     tools",
            "th down && th up",
            "th settings set sandbox.enabled true",
        ] {
            assert!(text.contains(needle), "{needle:?} missing from:\n{text}");
        }
        let r = resolver(&[], "").resolve("auto_mode").unwrap();
        assert!(explain_text(&r).contains("allowed     bypass, accept-edits, ask, deny"));
    }

    #[test]
    fn unknown_key_errors_suggest_the_right_one() {
        let err = resolver(&[], "").resolve("tailscale").unwrap_err();
        assert!(err.to_string().contains("tailscale.serve"), "{err}");
    }
}

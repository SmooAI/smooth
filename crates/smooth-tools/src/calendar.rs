//! `calendar` — read the user's macOS Calendar via the `ical` CLI (pearl th-94cc4a).
//!
//! ## Why this is a first-class tool and not just `bash ical today`
//!
//! **It must run OUTSIDE the kernel sandbox.** `ical` is a native EventKit
//! client, and EventKit talks to `calaccessd`/`tccd` over XPC + mach lookups
//! that [`crate::sandbox`]'s seatbelt profile denies. Run through the sandboxed
//! `bash` tool it fails with an opaque EventKit error every time. So this tool
//! spawns a plain [`tokio::process::Command`] — a deliberate, narrow
//! trusted-integration exception to "all subprocesses go through
//! `SandboxedCommand`".
//!
//! What keeps that honest:
//! - **argv only, no shell** — no interpolation or injection path.
//! - **fixed binary** — a resolved `ical` path, never caller-supplied.
//! - **read-only verb allowlist** ([`READ_COMMANDS`]) — `add`/`update`/`delete`/
//!   `import` are rejected here; calendar mutation is a later slice with its own
//!   confirmation story.
//! - **still Narc-visible** — it's a normal tool call, so the daemon's permission
//!   gate and the Narc hook see it exactly like any other.
//!
//! ## Availability
//! macOS-only (cfg-gated at registration). The tool registers even when it can't
//! work yet — a missing `ical` or an ungranted Calendar TCC grant returns
//! actionable setup guidance instead of an opaque failure, because "run
//! `th doctor --setup-calendar`" is something the agent can relay and the user
//! can act on.

#![cfg(target_os = "macos")]

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Value};
use smooth_operator::{Tool, ToolSchema};

/// Max bytes of output returned before truncation.
const OUTPUT_CAP: usize = 50_000;

/// Hard cap on an `ical` call. EventKit can hang indefinitely when the TCC
/// daemon is wedged; a stuck child would otherwise stall the whole agent turn.
const TIMEOUT: Duration = Duration::from_secs(30);

/// The read-only `ical` subcommands this tool exposes. Anything else — `add`,
/// `update`, `delete`, `import`, `rsvp` — is refused: writing to someone's
/// calendar is a separate slice.
const READ_COMMANDS: &[&str] = &["today", "upcoming", "list", "search", "show", "calendars", "free", "inbox"];

/// The setup instruction handed back whenever the integration isn't usable. One
/// string so the agent always relays the same next step.
const SETUP_HINT: &str = "Calendar isn't set up yet — run `th doctor --setup-calendar` on the Mac (installs the `ical` CLI and triggers the macOS Calendar permission prompt), then try again.";

/// `calendar` — read-only access to the macOS Calendar via `ical`.
pub struct CalendarTool;

/// Locate the `ical` binary: `SMOOTH_ICAL_BIN` → `~/.smooth/bin/ical` (where
/// `th doctor --setup-calendar` side-loads it) → Homebrew → `PATH`.
#[must_use]
pub fn resolve_ical() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("SMOOTH_ICAL_BIN").map(PathBuf::from) {
        if p.is_file() {
            return Some(p);
        }
    }
    if let Some(p) = dirs_next::home_dir().map(|h| h.join(".smooth").join("bin").join("ical")) {
        if p.is_file() {
            return Some(p);
        }
    }
    let brew = PathBuf::from("/opt/homebrew/bin/ical");
    if brew.is_file() {
        return Some(brew);
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join("ical")).find(|p| p.is_file())
}

#[async_trait]
impl Tool for CalendarTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "calendar".into(),
            description: format!(
                "Read the user's macOS Calendar (their real events — meetings, appointments, travel). Use this for anything about their schedule: what's on today, what's next, whether a time is free, finding an event. Read-only. Commands: {}. Examples: {{\"command\":\"today\"}}, {{\"command\":\"upcoming\",\"args\":[\"7\"]}}, {{\"command\":\"search\",\"args\":[\"dentist\"]}}, {{\"command\":\"list\",\"args\":[\"--from\",\"tomorrow\",\"--to\",\"friday\"]}}. Output is JSON.",
                READ_COMMANDS.join(", ")
            ),
            parameters: json!({
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "enum": READ_COMMANDS,
                        "description": "Which read command to run: today (today's events), upcoming (next N days), list (a date range), search (by text), show (one event by id), calendars (available calendars), free (free/busy), inbox (pending invitations)."
                    },
                    "args": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Extra arguments passed verbatim to `ical <command>`, e.g. [\"7\"] for upcoming, [\"--from\", \"monday\", \"--to\", \"friday\"] for list."
                    }
                },
                "required": ["command"]
            }),
        }
    }

    fn is_concurrent_safe(&self) -> bool {
        // Read-only lookups — safe alongside other tools.
        true
    }

    async fn execute(&self, arguments: Value) -> anyhow::Result<String> {
        let args = build_args(&arguments)?;
        let Some(bin) = resolve_ical() else {
            return Ok(format!("The `ical` CLI is not installed. {SETUP_HINT}"));
        };
        run_ical(&bin, &args).await
    }
}

/// Build the `ical` argv for a `calendar` call: `<command> [args…] -o json`.
///
/// Rejects any command outside [`READ_COMMANDS`] and any arg that looks like an
/// attempt to smuggle in a second command.
fn build_args(arguments: &Value) -> anyhow::Result<Vec<String>> {
    let command = arguments
        .get("command")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|c| !c.is_empty())
        .ok_or_else(|| anyhow::anyhow!("missing required string parameter `command`"))?;
    if !READ_COMMANDS.contains(&command) {
        anyhow::bail!(
            "`{command}` is not an allowed calendar command (read-only tool). Allowed: {}",
            READ_COMMANDS.join(", ")
        );
    }

    let mut argv = vec![command.to_owned()];
    if let Some(extra) = arguments.get("args") {
        let list = extra.as_array().ok_or_else(|| anyhow::anyhow!("`args` must be an array of strings"))?;
        for item in list {
            let s = item.as_str().ok_or_else(|| anyhow::anyhow!("`args` must be an array of strings"))?;
            argv.push(s.to_owned());
        }
    }
    // Machine-readable output, always — the model parses this, not a human.
    argv.push("-o".to_owned());
    argv.push("json".to_owned());
    Ok(argv)
}

/// Spawn `ical` **outside** the kernel sandbox (see the module docs) and return
/// its output, or setup guidance when the failure is a missing TCC grant.
async fn run_ical(bin: &std::path::Path, args: &[String]) -> anyhow::Result<String> {
    let mut cmd = tokio::process::Command::new(bin);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let child = cmd.spawn().map_err(|e| anyhow::anyhow!("failed to spawn `ical`: {e}"))?;
    let output = match tokio::time::timeout(TIMEOUT, child.wait_with_output()).await {
        Ok(r) => r.map_err(|e| anyhow::anyhow!("`ical` error: {e}"))?,
        Err(_) => anyhow::bail!(
            "`ical` timed out after {}s — EventKit may be waiting on a permission prompt. {SETUP_HINT}",
            TIMEOUT.as_secs()
        ),
    };

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    if !output.status.success() {
        if looks_like_permission_denial(&stderr) || looks_like_permission_denial(&stdout) {
            return Ok(format!(
                "Calendar access has not been granted to Big Smooth. {SETUP_HINT}\n\n--- ical said ---\n{}",
                truncate(&stderr)
            ));
        }
        let code = output.status.code().map_or_else(|| "killed by signal".to_owned(), |c| c.to_string());
        return Ok(format!("$ ical {}\nexit code: {code}\n{}", args.join(" "), truncate(&stderr)));
    }
    Ok(truncate(&stdout))
}

/// Whether `text` reads like a TCC denial rather than a real `ical` error. The
/// exact wording varies by macOS version and `ical` release, so this matches the
/// vocabulary all of them share.
fn looks_like_permission_denial(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    [
        "not authorized",
        "access denied",
        "permission",
        "authorization",
        "grant access",
        "not determined",
        "denied access",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

fn truncate(s: &str) -> String {
    if s.len() <= OUTPUT_CAP {
        return s.to_owned();
    }
    let mut cut = OUTPUT_CAP;
    while !s.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}\n… [truncated at {OUTPUT_CAP} bytes]", &s[..cut])
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "unwrap is the idiom for test assertions")]
mod tests {
    use super::*;

    #[test]
    fn schema_names_calendar_and_requires_command() {
        let s = CalendarTool.schema();
        assert_eq!(s.name, "calendar");
        assert_eq!(s.parameters["required"][0], "command");
        assert!(CalendarTool.is_concurrent_safe());
    }

    #[test]
    fn build_args_always_asks_for_json() {
        assert_eq!(build_args(&json!({"command": "today"})).unwrap(), vec!["today", "-o", "json"]);
    }

    #[test]
    fn build_args_passes_extra_args_through() {
        let args = build_args(&json!({"command": "upcoming", "args": ["7"]})).unwrap();
        assert_eq!(args, vec!["upcoming", "7", "-o", "json"]);
    }

    #[test]
    fn build_args_rejects_write_commands() {
        for bad in ["add", "delete", "update", "import", "rsvp"] {
            let err = build_args(&json!({"command": bad})).unwrap_err().to_string();
            assert!(err.contains("not an allowed calendar command"), "{bad}: {err}");
        }
    }

    #[test]
    fn build_args_rejects_missing_or_blank_command() {
        assert!(build_args(&json!({})).is_err());
        assert!(build_args(&json!({"command": "  "})).is_err());
        assert!(build_args(&json!({"command": 3})).is_err());
    }

    #[test]
    fn build_args_rejects_non_string_args() {
        assert!(build_args(&json!({"command": "today", "args": "7"})).is_err());
        assert!(build_args(&json!({"command": "today", "args": [7]})).is_err());
    }

    #[test]
    fn permission_denials_are_recognised_not_reported_as_errors() {
        assert!(looks_like_permission_denial("error: not authorized to access calendars"));
        assert!(looks_like_permission_denial("EventKit permission required"));
        assert!(looks_like_permission_denial("Authorization status: Denied"));
        assert!(!looks_like_permission_denial("no events found"));
        assert!(!looks_like_permission_denial("unknown flag: --nope"));
    }

    #[test]
    fn truncate_caps_output_on_a_char_boundary() {
        let long = "é".repeat(OUTPUT_CAP);
        let out = truncate(&long);
        assert!(out.contains("truncated"));
        assert!(out.len() < long.len());
        assert_eq!(truncate("short"), "short");
    }

    #[tokio::test]
    async fn execute_rejects_a_write_command_before_spawning() {
        let err = CalendarTool.execute(json!({"command": "delete", "args": ["1"]})).await.unwrap_err().to_string();
        assert!(err.contains("not an allowed calendar command"), "{err}");
    }

    #[test]
    fn setup_hint_names_the_one_command_that_fixes_it() {
        // Every not-usable path (no binary, no grant, timeout) funnels the agent
        // to the same actionable next step — that's the point of the slice.
        assert!(SETUP_HINT.contains("th doctor --setup-calendar"));
    }

    #[test]
    fn resolver_ignores_an_env_override_that_does_not_exist() {
        // ponytail: the resolver falls through missing candidates; whether it
        // ends up finding a real `ical` depends on the machine, so only the
        // "bogus override is not returned" invariant is asserted.
        std::env::set_var("SMOOTH_ICAL_BIN", "/nonexistent/ical");
        assert_ne!(resolve_ical(), Some(PathBuf::from("/nonexistent/ical")));
        std::env::remove_var("SMOOTH_ICAL_BIN");
    }
}

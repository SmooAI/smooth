//! `calendar` — read and adjust the user's macOS Calendar via the `ical` CLI
//! (pearl th-94cc4a).
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
//! - **verb allowlist** ([`COMMANDS`]) — reads plus `add`/`update`/`delete`;
//!   anything else (`import`, `rsvp`, `skills`, …) is refused.
//! - **still Narc-visible** — it's a normal tool call, so the daemon's permission
//!   gate and the Narc hook see it exactly like any other. Note what that means
//!   for writes: under the daemon's default `AutoMode::Bypass` a calendar write
//!   runs unprompted, same as `write_file`. Deliberate — `SMOOTH_AUTO_MODE=ask`
//!   is the knob for a stricter posture.
//!
//! ## Writes and `ical`'s interactive modes
//! `ical` is built for a human at a terminal: `-i` walks a guided prompt,
//! `update`/`delete` with no event argument open a picker, and `delete` asks for
//! confirmation. The daemon spawns it with **null stdin**, so every one of those
//! would stall until the timeout. [`build_args`] therefore rejects `-i`, requires
//! an event identifier for `update`/`delete`, and adds `--force` to `delete`
//! (the confirmation can't be answered, and the permission gate already asked
//! the question that matters).
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

/// The `ical` subcommands this tool exposes — reads first, then the three
/// mutations. Anything else (`import`, `rsvp`, `skills`, `join`, `export`) is
/// refused: an allowlist, not a denylist, so a new `ical` release can't quietly
/// widen what the agent can do.
const COMMANDS: &[&str] = &[
    "today",
    "upcoming",
    "list",
    "search",
    "show",
    "calendars",
    "free",
    "inbox",
    "add",
    "update",
    "delete",
];

/// The setup instruction handed back whenever the integration isn't usable. One
/// string so the agent always relays the same next step.
const SETUP_HINT: &str = "Calendar isn't set up yet — run `th doctor --setup-calendar` on the Mac (installs the `ical` CLI and triggers the macOS Calendar permission prompt), then try again.";

/// `calendar` — read and adjust the macOS Calendar via `ical`.
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
                "Read AND adjust the user's real macOS Calendar — meetings, appointments, travel. Use it for anything about their schedule (what's on today, what's next, is a time free, find an event) and to change it (book something, move it, cancel it). Commands: {}. Reads: {{\"command\":\"today\"}}, {{\"command\":\"upcoming\",\"args\":[\"7\"]}}, {{\"command\":\"search\",\"args\":[\"dentist\"]}}, {{\"command\":\"list\",\"args\":[\"--from\",\"tomorrow\",\"--to\",\"friday\"]}}. Writes: {{\"command\":\"add\",\"args\":[\"Dentist\",\"-s\",\"tomorrow 2pm\",\"-e\",\"tomorrow 3pm\",\"-l\",\"Main St\"]}}, {{\"command\":\"update\",\"args\":[\"<event-id>\",\"-s\",\"friday 10am\"]}}, {{\"command\":\"delete\",\"args\":[\"<event-id>\"]}}. Get the id from a read first — `update` and `delete` REQUIRE one. Output is JSON.",
                COMMANDS.join(", ")
            ),
            parameters: json!({
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "enum": COMMANDS,
                        "description": "today (today's events), upcoming (next N days), list (a date range), search (by text), show (one event by id), calendars (available calendars), free (free/busy), inbox (pending invitations), add (create an event), update (change one), delete (remove one)."
                    },
                    "args": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Arguments passed verbatim to `ical <command>`. Reads: [\"7\"] for upcoming, [\"--from\",\"monday\",\"--to\",\"friday\"] for list. add: the title, then -s/--start (required), -e/--end, -l/--location, -n/--notes, -c/--calendar, -a/--all-day, --invite <email>, --alert 15m, -r/--repeat daily|weekly|monthly|yearly. update: the event id, then the same field flags. delete: the event id. Natural-language dates work (\"tomorrow 2pm\", \"friday\")."
                    }
                },
                "required": ["command"]
            }),
        }
    }

    fn is_concurrent_safe(&self) -> bool {
        // ponytail: the flag is per-tool, not per-call, and this one can now
        // mutate the calendar — so it's serialized. Split into `calendar` +
        // `calendar_write` if parallel reads ever actually matter.
        false
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
/// Rejects any command outside [`COMMANDS`], plus the shapes that would hang on
/// the daemon's null stdin (see the module docs).
fn build_args(arguments: &Value) -> anyhow::Result<Vec<String>> {
    let command = arguments
        .get("command")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|c| !c.is_empty())
        .ok_or_else(|| anyhow::anyhow!("missing required string parameter `command`"))?;
    if !COMMANDS.contains(&command) {
        anyhow::bail!("`{command}` is not an allowed calendar command. Allowed: {}", COMMANDS.join(", "));
    }

    let mut extra: Vec<String> = Vec::new();
    if let Some(list) = arguments.get("args") {
        let list = list.as_array().ok_or_else(|| anyhow::anyhow!("`args` must be an array of strings"))?;
        for item in list {
            let s = item.as_str().ok_or_else(|| anyhow::anyhow!("`args` must be an array of strings"))?;
            extra.push(s.to_owned());
        }
    }

    // Interactive mode walks a guided prompt on stdin — which is /dev/null here,
    // so it would just burn the timeout. Same for the pickers below.
    if extra.iter().any(|a| a == "-i" || a == "--interactive") {
        anyhow::bail!("interactive mode isn't available to the agent — pass the fields as flags instead (e.g. -s \"tomorrow 2pm\")");
    }
    // `update`/`delete` with no event argument open an interactive picker.
    if matches!(command, "update" | "delete") && !has_event_ref(&extra) {
        anyhow::bail!("`{command}` needs the event to act on — run a read first (e.g. today/search) and pass its id as the first arg");
    }

    let mut argv = vec![command.to_owned()];
    argv.extend(extra);
    // `delete` prompts for confirmation on a TTY it doesn't have. The decision
    // the user actually gets to make is the permission gate on this tool call.
    if command == "delete" && !argv.iter().any(|a| a == "-f" || a == "--force") {
        argv.push("--force".to_owned());
    }
    // Machine-readable output, always — the model parses this, not a human.
    argv.push("-o".to_owned());
    argv.push("json".to_owned());
    Ok(argv)
}

/// Whether `args` names an event: `--id <id>` anywhere, or a leading positional
/// (`ical update [number or id] [flags]` — the id comes first).
///
/// Deliberately only the FIRST arg: a bare word later in the list is a flag's
/// value (`-s friday`), not an event id, and treating it as one would let an
/// `update` with no target through to the interactive picker.
fn has_event_ref(args: &[String]) -> bool {
    args.iter().any(|a| a == "--id") || args.first().is_some_and(|a| !a.starts_with('-'))
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
        assert!(!CalendarTool.is_concurrent_safe(), "the tool mutates now, so it must serialize");
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
    fn build_args_rejects_commands_outside_the_allowlist() {
        for bad in ["import", "rsvp", "skills", "join", "export", "completion"] {
            let err = build_args(&json!({"command": bad})).unwrap_err().to_string();
            assert!(err.contains("not an allowed calendar command"), "{bad}: {err}");
        }
    }

    #[test]
    fn build_args_allows_creating_an_event() {
        let args = build_args(&json!({"command": "add", "args": ["Dentist", "-s", "tomorrow 2pm"]})).unwrap();
        assert_eq!(args, vec!["add", "Dentist", "-s", "tomorrow 2pm", "-o", "json"]);
    }

    #[test]
    fn build_args_allows_updating_by_id() {
        let args = build_args(&json!({"command": "update", "args": ["EV-1", "-s", "friday 10am"]})).unwrap();
        assert_eq!(args, vec!["update", "EV-1", "-s", "friday 10am", "-o", "json"]);
        // `--id EV-1` is the other accepted way to name the event.
        let by_flag = build_args(&json!({"command": "update", "args": ["--id", "EV-1", "-l", "Home"]})).unwrap();
        assert_eq!(by_flag, vec!["update", "--id", "EV-1", "-l", "Home", "-o", "json"]);
    }

    #[test]
    fn delete_gets_force_because_the_daemon_has_no_tty_to_confirm_on() {
        let args = build_args(&json!({"command": "delete", "args": ["EV-1"]})).unwrap();
        assert_eq!(args, vec!["delete", "EV-1", "--force", "-o", "json"]);
        // Already forced by the caller — don't add it twice.
        let explicit = build_args(&json!({"command": "delete", "args": ["EV-1", "--force"]})).unwrap();
        assert_eq!(explicit.iter().filter(|a| *a == "--force").count(), 1);
    }

    #[test]
    fn update_and_delete_require_naming_an_event() {
        // Bare, or flags only: both would open ical's interactive picker and
        // hang on null stdin until the timeout. `["-s", "friday"]` is the
        // regression case — "friday" is a flag VALUE, not an event id.
        for args in [
            json!({"command": "delete"}),
            json!({"command": "update", "args": ["-s", "friday"]}),
            json!({"command": "delete", "args": ["--span", "all"]}),
        ] {
            let err = build_args(&args).unwrap_err().to_string();
            assert!(err.contains("needs the event to act on"), "{args}: {err}");
        }
    }

    #[test]
    fn interactive_mode_is_refused_on_every_command() {
        for flag in ["-i", "--interactive"] {
            let err = build_args(&json!({"command": "add", "args": [flag]})).unwrap_err().to_string();
            assert!(err.contains("interactive mode isn't available"), "{flag}: {err}");
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
    async fn execute_rejects_a_disallowed_command_before_spawning() {
        let err = CalendarTool
            .execute(json!({"command": "import", "args": ["x.ics"]}))
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("not an allowed calendar command"), "{err}");
    }

    #[test]
    fn the_write_commands_are_reachable() {
        for w in ["add", "update", "delete"] {
            assert!(COMMANDS.contains(&w), "{w} must be reachable");
        }
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

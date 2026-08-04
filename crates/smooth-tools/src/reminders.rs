//! `reminders` — read and adjust the user's macOS Reminders via EventKit
//! (pearl th-94cc4a, the reminders slice of the calendar work).
//!
//! ## Why this is a first-class tool and not just `bash`
//!
//! **It must run OUTSIDE the kernel sandbox.** EventKit talks to
//! `remindd`/`tccd` over XPC + mach lookups that [`crate::sandbox`]'s seatbelt
//! profile denies. So, like [`crate::calendar`], this is a deliberate, narrow
//! trusted-integration exception to "all subprocesses go through
//! `SandboxedCommand`" — except here there is **no subprocess at all**: the
//! reads and writes are in-process EventKit calls
//! ([`smooth_menubar::reminders`], the workspace's objc2 quarantine crate).
//! `ical` is calendar-only, and `osascript` → Reminders.app would swap the
//! EventKit grant for a flakier Automation grant that needs the app running.
//!
//! What keeps that honest:
//! - **no shell, no argv, no binary** — typed arguments straight into a
//!   framework call, so there is no interpolation or injection path at all.
//! - **verb allowlist** ([`VERBS`]) — `list`, `add`, `complete`. No delete: a
//!   reminder the agent shouldn't have made is completed, not vanished.
//! - **still Narc-visible** — a normal tool call, so the daemon's permission
//!   gate and the Narc hook see it exactly like any other. Note what that means
//!   for writes: under the daemon's default `AutoMode::Bypass` an `add` runs
//!   unprompted, same as `write_file`. Deliberate — `SMOOTH_AUTO_MODE=ask` is
//!   the knob for a stricter posture.
//!
//! ## Availability
//! macOS-only (cfg-gated at registration). The tool registers even when it can't
//! work yet — an ungranted Reminders TCC grant returns actionable setup guidance
//! instead of an empty list, because "run `th doctor --setup-reminders`" is
//! something the agent can relay and the user can act on. Reminders is a
//! **separate** grant from Calendar; having one says nothing about the other.

#![cfg(target_os = "macos")]

use async_trait::async_trait;
use chrono::{NaiveDate, NaiveDateTime};
use serde_json::{json, Value};
use smooth_menubar::eventkit::reminders_access;
use smooth_menubar::reminders::{self as ek, Due, Reminder};
use smooth_menubar::setup::{initiate, Grant};
use smooth_operator::{Tool, ToolSchema};

/// The verbs this tool exposes. An allowlist, not a denylist.
const VERBS: &[&str] = &["list", "add", "complete"];

/// Hard cap on returned rows. `status:"all"` over a long-lived Reminders
/// database can run to thousands of completed items; the agent needs the recent
/// shape of the list, not the archive.
const MAX_ITEMS: usize = 200;

/// The setup instruction handed back whenever the integration isn't usable. One
/// string so the agent always relays the same next step.
const SETUP_HINT: &str =
    "Reminders isn't set up yet — run `th doctor --setup-reminders` on the Mac (triggers the macOS Reminders permission prompt), then try again.";

/// `reminders` — read and adjust the macOS Reminders database.
pub struct RemindersTool;

#[async_trait]
impl Tool for RemindersTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "reminders".into(),
            description: "Read AND adjust the user's real macOS Reminders — todos, tasks, shopping lists. Use it for anything about what they have to do (what's on my list, what's due, is X on there) and to change it (add a todo, mark one done). Verbs: list, add, complete. Examples: {\"verb\":\"list\"}, {\"verb\":\"list\",\"list\":\"Groceries\"}, {\"verb\":\"list\",\"status\":\"all\"}, {\"verb\":\"add\",\"title\":\"Buy milk\",\"due\":\"2026-08-05 09:00\",\"list\":\"Groceries\"}, {\"verb\":\"complete\",\"id\":\"<id-from-a-list>\"}. Due dates are absolute — \"YYYY-MM-DD\" or \"YYYY-MM-DD HH:MM\", no natural language (use the current_datetime tool to resolve \"tomorrow\" first). Output is JSON.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "verb": {
                        "type": "string",
                        "enum": VERBS,
                        "description": "list (read reminders), add (create one), complete (mark one done)."
                    },
                    "status": {
                        "type": "string",
                        "enum": ["open", "all"],
                        "description": "list only. `open` (default) returns unfinished reminders; `all` also includes completed ones."
                    },
                    "list": {
                        "type": "string",
                        "description": "The reminder list name. On `list` it filters; on `add` it picks where the reminder lands (default list when omitted). Case-insensitive."
                    },
                    "title": {
                        "type": "string",
                        "description": "add only, required. The reminder text."
                    },
                    "due": {
                        "type": "string",
                        "description": "add only, optional. Absolute due date: \"YYYY-MM-DD\" for a whole day, or \"YYYY-MM-DD HH:MM\" (24-hour, local time) for a time. Natural language is NOT parsed."
                    },
                    "id": {
                        "type": "string",
                        "description": "complete only, required. The `id` of a reminder from a previous `list` call."
                    }
                },
                "required": ["verb"]
            }),
        }
    }

    fn is_concurrent_safe(&self) -> bool {
        // The tool mutates a shared OS database, and every EventKit call here is
        // blocking — serialize it.
        false
    }

    async fn execute(&self, arguments: Value) -> anyhow::Result<String> {
        let call = Call::parse(&arguments)?;
        if !reminders_access().granted() {
            // Ask for it right here (once per session) rather than sending the
            // user off to `th doctor` — the prompt has to come from this
            // process for the grant to land on it (pearl th-ba764e).
            let next_step = initiate(Grant::Reminders).unwrap_or(SETUP_HINT);
            return Ok(format!("Reminders access has not been granted to Big Smooth. {next_step}"));
        }
        // EventKit blocks (see `smooth_menubar::reminders`) — keep it off the
        // async runtime's worker threads.
        let outcome = tokio::task::spawn_blocking(move || call.run()).await?;
        Ok(match outcome {
            Ok(text) => text,
            // A failed EventKit call is an answer, not a tool crash: the model
            // can act on "no list named X. Lists: …" but not on a hard error.
            Err(e) => format!("{e:#}"),
        })
    }
}

/// A validated `reminders` call — parsing is separated from execution so the
/// argument rules are testable without a TCC grant.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Call {
    List { include_completed: bool, list: Option<String> },
    Add { title: String, due: Option<Due>, list: Option<String> },
    Complete { id: String },
}

impl Call {
    /// Validate the tool arguments into a [`Call`].
    fn parse(arguments: &Value) -> anyhow::Result<Self> {
        let verb = str_arg(arguments, "verb").ok_or_else(|| anyhow::anyhow!("missing required string parameter `verb`"))?;
        if !VERBS.contains(&verb.as_str()) {
            anyhow::bail!("`{verb}` is not an allowed reminders verb. Allowed: {}", VERBS.join(", "));
        }
        let list = str_arg(arguments, "list");
        match verb.as_str() {
            "list" => {
                let include_completed = match str_arg(arguments, "status").as_deref() {
                    None | Some("open") => false,
                    Some("all") => true,
                    Some(other) => anyhow::bail!("`status` must be \"open\" or \"all\", got `{other}`"),
                };
                Ok(Self::List { include_completed, list })
            }
            "add" => {
                let title = str_arg(arguments, "title").ok_or_else(|| anyhow::anyhow!("`add` needs a `title`"))?;
                let due = str_arg(arguments, "due").map(|d| parse_due(&d)).transpose()?;
                Ok(Self::Add { title, due, list })
            }
            _ => {
                let id = str_arg(arguments, "id")
                    .ok_or_else(|| anyhow::anyhow!("`complete` needs the `id` of the reminder — run a `list` first and pass the `id` from it"))?;
                Ok(Self::Complete { id })
            }
        }
    }

    /// Execute against EventKit. **Blocks** — callers must be off the runtime.
    fn run(self) -> anyhow::Result<String> {
        let value = match self {
            Self::List { include_completed, list } => {
                let found = ek::list(include_completed, list.as_deref())?;
                let total = found.len();
                let rows: Vec<Value> = found.iter().take(MAX_ITEMS).map(render).collect();
                json!({
                    "count": rows.len(),
                    "total": total,
                    "truncated": total > rows.len(),
                    "reminders": rows,
                })
            }
            Self::Add { title, due, list } => {
                let made = ek::add(&title, due, list.as_deref())?;
                json!({ "added": render(&made) })
            }
            Self::Complete { id } => {
                let done = ek::complete(&id)?;
                json!({ "completed": render(&done) })
            }
        };
        Ok(value.to_string())
    }
}

/// A non-empty trimmed string argument, if present.
fn str_arg(arguments: &Value, key: &str) -> Option<String> {
    arguments
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
}

/// One reminder as the model sees it.
fn render(r: &Reminder) -> Value {
    json!({
        "id": r.id,
        "title": r.title,
        "list": r.list,
        "completed": r.completed,
        "due": r.due.map(format_due),
        "notes": r.notes,
    })
}

/// [`Due`] → the same string shape [`parse_due`] accepts, so a `list` result can
/// be fed straight back into an `add`.
fn format_due(d: Due) -> String {
    match d.time {
        Some((h, m)) => format!("{:04}-{:02}-{:02} {h:02}:{m:02}", d.year, d.month, d.day),
        None => format!("{:04}-{:02}-{:02}", d.year, d.month, d.day),
    }
}

/// Parse an absolute due date: `YYYY-MM-DD` or `YYYY-MM-DD HH:MM` (`T` accepted
/// in place of the space, and trailing `:SS` tolerated for ISO-8601 habits).
///
/// ponytail: deliberately NO natural-language parsing. `ical` gets "tomorrow
/// 2pm" from a Go library we don't have here, and the model already has the
/// `current_datetime` tool to resolve relative dates itself — one honest format
/// beats a half-working date guesser that books things on the wrong day.
fn parse_due(s: &str) -> anyhow::Result<Due> {
    use chrono::{Datelike, Timelike};
    let s = s.trim();
    if let Ok(d) = NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        return Ok(Due {
            year: d.year(),
            month: d.month(),
            day: d.day(),
            time: None,
        });
    }
    for fmt in ["%Y-%m-%d %H:%M", "%Y-%m-%dT%H:%M", "%Y-%m-%d %H:%M:%S", "%Y-%m-%dT%H:%M:%S"] {
        if let Ok(dt) = NaiveDateTime::parse_from_str(s, fmt) {
            return Ok(Due {
                year: dt.year(),
                month: dt.month(),
                day: dt.day(),
                time: Some((dt.hour(), dt.minute())),
            });
        }
    }
    anyhow::bail!(
        "`{s}` isn't a due date I can read — use \"YYYY-MM-DD\" or \"YYYY-MM-DD HH:MM\" (resolve relative dates with the current_datetime tool first)"
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "unwrap is the idiom for test assertions")]
mod tests {
    use super::*;

    #[test]
    fn schema_names_reminders_and_requires_a_verb() {
        let s = RemindersTool.schema();
        assert_eq!(s.name, "reminders");
        assert_eq!(s.parameters["required"][0], "verb");
        assert!(!RemindersTool.is_concurrent_safe(), "the tool mutates, so it must serialize");
    }

    #[test]
    fn the_schema_advertises_exactly_the_allowed_verbs() {
        let s = RemindersTool.schema();
        let enumerated: Vec<String> = s.parameters["properties"]["verb"]["enum"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_owned())
            .collect();
        assert_eq!(enumerated, VERBS);
        // No delete verb, on purpose — completing is the reversible answer.
        assert!(!VERBS.contains(&"delete"), "{VERBS:?}");
    }

    #[test]
    fn list_defaults_to_open_reminders_across_every_list() {
        assert_eq!(
            Call::parse(&json!({"verb": "list"})).unwrap(),
            Call::List {
                include_completed: false,
                list: None
            }
        );
    }

    #[test]
    fn list_honours_status_and_list_filters() {
        assert_eq!(
            Call::parse(&json!({"verb": "list", "status": "all", "list": "Groceries"})).unwrap(),
            Call::List {
                include_completed: true,
                list: Some("Groceries".to_owned())
            }
        );
        let err = Call::parse(&json!({"verb": "list", "status": "done"})).unwrap_err().to_string();
        assert!(err.contains("must be \"open\" or \"all\""), "{err}");
    }

    #[test]
    fn add_requires_a_title() {
        let err = Call::parse(&json!({"verb": "add"})).unwrap_err().to_string();
        assert!(err.contains("needs a `title`"), "{err}");
        // Blank is missing, not a reminder called "   ".
        assert!(Call::parse(&json!({"verb": "add", "title": "   "})).is_err());
    }

    #[test]
    fn add_parses_both_due_date_shapes() {
        let Call::Add { due, title, list } = Call::parse(&json!({"verb": "add", "title": "Buy milk", "due": "2026-08-05"})).unwrap() else {
            panic!("expected an add")
        };
        assert_eq!(title, "Buy milk");
        assert_eq!(list, None);
        assert_eq!(
            due,
            Some(Due {
                year: 2026,
                month: 8,
                day: 5,
                time: None
            })
        );

        let Call::Add { due, .. } = Call::parse(&json!({"verb": "add", "title": "Standup", "due": "2026-08-05 09:30"})).unwrap() else {
            panic!("expected an add")
        };
        assert_eq!(
            due,
            Some(Due {
                year: 2026,
                month: 8,
                day: 5,
                time: Some((9, 30))
            })
        );
    }

    #[test]
    fn natural_language_dates_are_refused_with_the_fix() {
        // The failure mode this guards: silently booking "tomorrow" as no due
        // date at all, or worse, on a wrong day.
        for bad in ["tomorrow", "tomorrow 2pm", "friday", "next week", "08/05/2026"] {
            let err = Call::parse(&json!({"verb": "add", "title": "x", "due": bad})).unwrap_err().to_string();
            assert!(err.contains("current_datetime"), "{bad}: {err}");
        }
    }

    #[test]
    fn iso_8601_habits_still_parse() {
        for s in ["2026-08-05T09:30", "2026-08-05 09:30:00", "2026-08-05T09:30:15"] {
            assert_eq!(parse_due(s).unwrap().time, Some((9, 30)), "{s}");
        }
    }

    #[test]
    fn complete_requires_an_id() {
        let err = Call::parse(&json!({"verb": "complete"})).unwrap_err().to_string();
        assert!(err.contains("run a `list` first"), "{err}");
        assert_eq!(
            Call::parse(&json!({"verb": "complete", "id": "x-1"})).unwrap(),
            Call::Complete { id: "x-1".to_owned() }
        );
    }

    #[test]
    fn verbs_outside_the_allowlist_are_refused() {
        for bad in ["delete", "remove", "update", "export", "share", ""] {
            assert!(Call::parse(&json!({"verb": bad})).is_err(), "{bad} must be refused");
        }
        assert!(Call::parse(&json!({})).is_err(), "a missing verb must be refused");
        assert!(Call::parse(&json!({"verb": 7})).is_err(), "a non-string verb must be refused");
    }

    #[test]
    fn a_due_date_round_trips_through_the_rendered_string() {
        // Load-bearing: a `due` from a `list` result must be re-parseable, or
        // "move this to the same time next week" silently loses the time.
        for s in ["2026-08-05", "2026-08-05 09:30"] {
            assert_eq!(format_due(parse_due(s).unwrap()), s);
        }
    }

    #[test]
    fn rendering_exposes_the_id_the_complete_verb_needs() {
        let r = Reminder {
            id: "abc-123".to_owned(),
            title: "Buy milk".to_owned(),
            list: "Groceries".to_owned(),
            completed: false,
            due: Some(Due {
                year: 2026,
                month: 8,
                day: 5,
                time: Some((9, 30)),
            }),
            notes: None,
        };
        let v = render(&r);
        assert_eq!(v["id"], "abc-123");
        assert_eq!(v["due"], "2026-08-05 09:30");
        assert_eq!(v["notes"], Value::Null);
        assert_eq!(v["completed"], false);
    }

    #[test]
    fn setup_hint_names_the_one_command_that_fixes_it() {
        // Every not-usable path funnels the agent to the same actionable step.
        assert!(SETUP_HINT.contains("th doctor --setup-reminders"));
    }

    #[tokio::test]
    async fn execute_rejects_a_bad_verb_before_touching_eventkit() {
        let err = RemindersTool.execute(json!({"verb": "delete", "id": "x"})).await.unwrap_err().to_string();
        assert!(err.contains("not an allowed reminders verb"), "{err}");
    }

    #[tokio::test]
    async fn execute_without_a_grant_returns_the_setup_hint_not_an_empty_list() {
        // A test binary is never TCC-granted, so this is the ungranted path. An
        // empty list here would make Big Smooth claim the user has no todos.
        if reminders_access().granted() {
            return; // granted on this machine — the ungranted path isn't reachable
        }
        let out = RemindersTool.execute(json!({"verb": "list"})).await.unwrap();
        assert!(out.contains("th doctor --setup-reminders"), "{out}");
    }
}

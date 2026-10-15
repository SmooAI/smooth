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
//! - **verb allowlist** ([`VERBS`]) — `lists`, `list`, `add`, `complete`,
//!   `create_list`, `move`, `update` (SMOODEV-3734 added the last three, so
//!   "categorize these into new lists" is one tool's job). Deleting a reminder
//!   is NOT a verb here: it lives on its own [`RemindersDeleteTool`], for the
//!   same reason `calendar_delete` does — core's write-confirmation HITL gates
//!   by tool name, so the one irreversible mutation gets its own name and the
//!   daemon lists it in `CONFIRM_TOOLS`. Deleting a *list* isn't offered at all.
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
use smooth_menubar::reminders::{self as ek, Changes, Due, Reminder};
use smooth_menubar::setup::{initiate, Grant};
use smooth_operator::{Tool, ToolSchema};

/// The verbs this tool exposes. An allowlist, not a denylist.
const VERBS: &[&str] = &["lists", "list", "add", "complete", "create_list", "move", "update"];

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
            description: "Read AND adjust the user's real macOS Reminders — todos, tasks, shopping lists. Use it for anything about what they have to do (what's on my list, what's due, is X on there) and to change it. Verbs: lists (names of every list), list (reminders), add, complete, create_list (a new list on the same account as the default, or as the `like` list), move (a reminder to another list, keeping every field), update (title / notes / due / priority). Examples: {\"verb\":\"lists\"}, {\"verb\":\"list\",\"list\":\"Groceries\"}, {\"verb\":\"list\",\"status\":\"all\"}, {\"verb\":\"add\",\"title\":\"Buy milk\",\"due\":\"2026-08-05 09:00\",\"list\":\"Groceries\"}, {\"verb\":\"complete\",\"id\":\"<id>\"}, {\"verb\":\"create_list\",\"name\":\"Cleaning\",\"like\":\"House\"}, {\"verb\":\"move\",\"id\":\"<id>\",\"list\":\"Cleaning\"}, {\"verb\":\"update\",\"id\":\"<id>\",\"priority\":\"high\",\"notes\":\"\"}. `id`s come from a `list` call. To categorize reminders, create the lists then `move` each one. NOT supported here: deleting a reminder (use the separate `reminders_delete` tool, which asks the user first), and renaming or deleting a list (fall back to `bash` + `osascript` for those). Due dates are absolute — \"YYYY-MM-DD\" or \"YYYY-MM-DD HH:MM\", no natural language (use the get_current_datetime tool to resolve \"tomorrow\" first). Output is JSON.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "verb": {
                        "type": "string",
                        "enum": VERBS,
                        "description": "lists (list names), list (read reminders), add (create one), complete (mark one done), create_list (new list), move (reminder → another list), update (edit a reminder's fields)."
                    },
                    "status": {
                        "type": "string",
                        "enum": ["open", "all"],
                        "description": "list only. `open` (default) returns unfinished reminders; `all` also includes completed ones."
                    },
                    "list": {
                        "type": "string",
                        "description": "A reminder list name, case-insensitive. On `list` it filters; on `add` it picks where the reminder lands (default list when omitted); on `move` it's the destination (required). An unknown name errors with the real list names."
                    },
                    "name": {
                        "type": "string",
                        "description": "create_list only, required. The new list's name; must not match an existing list."
                    },
                    "like": {
                        "type": "string",
                        "description": "create_list only, optional. An existing list whose account (iCloud, Exchange, …) the new list joins. Default: the default list's account."
                    },
                    "title": {
                        "type": "string",
                        "description": "add (required) / update (optional). The reminder text."
                    },
                    "notes": {
                        "type": "string",
                        "description": "update only, optional. New notes; \"\" clears them."
                    },
                    "due": {
                        "type": "string",
                        "description": "add / update, optional. Absolute due date: \"YYYY-MM-DD\" for a whole day, or \"YYYY-MM-DD HH:MM\" (24-hour, local time) for a time. On update, \"\" clears it. Natural language is NOT parsed."
                    },
                    "priority": {
                        "type": "string",
                        "enum": ["none", "low", "medium", "high"],
                        "description": "update only, optional. The reminder's priority."
                    },
                    "id": {
                        "type": "string",
                        "description": "complete / move / update, required. The `id` of a reminder from a previous `list` call."
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
        run_granted(Call::parse(&arguments)?).await
    }
}

/// `reminders_delete` — delete one reminder for good. Its own tool **only** so
/// it can be confirmation-gated (the daemon lists it in `CONFIRM_TOOLS`); see
/// the module docs.
pub struct RemindersDeleteTool;

#[async_trait]
impl Tool for RemindersDeleteTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "reminders_delete".into(),
            description: "Permanently delete one reminder from the user's macOS Reminders. This asks the user to confirm before it runs. Prefer `reminders` complete for a todo that's done; delete is for mistakes and duplicates. Pass the reminder `id` from a `reminders` list call: {\"id\":\"<id>\"}. Output is JSON.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "id": {
                        "type": "string",
                        "description": "The `id` of the reminder to delete, from a previous `reminders` list call."
                    }
                },
                "required": ["id"]
            }),
        }
    }

    fn is_concurrent_safe(&self) -> bool {
        false
    }

    async fn execute(&self, arguments: Value) -> anyhow::Result<String> {
        run_granted(Call::Delete {
            id: id_arg(&arguments, "reminders_delete")?,
        })
        .await
    }
}

/// Run a parsed call once the Reminders grant is in place, or hand back the
/// setup step when it isn't.
async fn run_granted(call: Call) -> anyhow::Result<String> {
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

/// A validated `reminders` call — parsing is separated from execution so the
/// argument rules are testable without a TCC grant.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Call {
    Lists,
    List {
        include_completed: bool,
        list: Option<String>,
    },
    Add {
        title: String,
        due: Option<Due>,
        list: Option<String>,
    },
    Complete {
        id: String,
    },
    CreateList {
        name: String,
        like: Option<String>,
    },
    Move {
        id: String,
        list: String,
    },
    Update {
        id: String,
        changes: Changes,
    },
    /// Only reachable through [`RemindersDeleteTool`] — never parsed from a
    /// `reminders` verb.
    Delete {
        id: String,
    },
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
            "lists" => Ok(Self::Lists),
            "complete" => Ok(Self::Complete {
                id: id_arg(arguments, "complete")?,
            }),
            "create_list" => {
                let name = str_arg(arguments, "name").ok_or_else(|| anyhow::anyhow!("`create_list` needs the new list's `name`"))?;
                Ok(Self::CreateList {
                    name,
                    like: str_arg(arguments, "like"),
                })
            }
            "move" => {
                let id = id_arg(arguments, "move")?;
                let list = list.ok_or_else(|| anyhow::anyhow!("`move` needs the destination `list` — run `lists` to see the names"))?;
                Ok(Self::Move { id, list })
            }
            _ => {
                let id = id_arg(arguments, "update")?;
                let title = match arguments.get("title") {
                    None => None,
                    Some(_) => Some(str_arg(arguments, "title").ok_or_else(|| anyhow::anyhow!("`title` can't be blank"))?),
                };
                let changes = Changes {
                    title,
                    notes: clearable_arg(arguments, "notes"),
                    due: clearable_arg(arguments, "due").map(|d| d.map(|d| parse_due(&d)).transpose()).transpose()?,
                    priority: str_arg(arguments, "priority").map(|p| parse_priority(&p)).transpose()?,
                };
                if changes.is_empty() {
                    anyhow::bail!("`update` needs at least one of `title`, `notes`, `due`, `priority`");
                }
                Ok(Self::Update { id, changes })
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
            Self::Lists => json!({ "lists": ek::lists()? }),
            Self::CreateList { name, like } => {
                let made = ek::create_list(&name, like.as_deref())?;
                json!({ "created_list": { "name": made.name, "account": made.account } })
            }
            Self::Move { id, list } => {
                let moved = ek::move_to(&id, &list)?;
                json!({ "moved": render(&moved) })
            }
            Self::Update { id, changes } => {
                let updated = ek::update(&id, &changes)?;
                json!({ "updated": render(&updated) })
            }
            Self::Delete { id } => {
                let gone = ek::delete(&id)?;
                json!({ "deleted": render(&gone) })
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

/// The required `id` argument for `verb`.
fn id_arg(arguments: &Value, verb: &str) -> anyhow::Result<String> {
    str_arg(arguments, "id").ok_or_else(|| anyhow::anyhow!("`{verb}` needs the `id` of the reminder — run a `list` first and pass the `id` from it"))
}

/// A field `update` can clear: absent → `None` (leave it), `""` → `Some(None)`
/// (clear it), anything else → `Some(Some(value))`.
#[allow(clippy::option_option, reason = "three states on purpose: leave, clear, set — the shape `Changes` takes")]
fn clearable_arg(arguments: &Value, key: &str) -> Option<Option<String>> {
    let raw = arguments.get(key)?.as_str()?;
    Some(Some(raw.trim().to_owned()).filter(|s| !s.is_empty()))
}

/// `none`/`low`/`medium`/`high` → EventKit's 0–9 priority scale (the values
/// Reminders.app itself writes: 0, 9, 5, 1).
fn parse_priority(p: &str) -> anyhow::Result<u8> {
    match p.to_ascii_lowercase().as_str() {
        "none" => Ok(0),
        "low" => Ok(9),
        "medium" => Ok(5),
        "high" => Ok(1),
        other => anyhow::bail!("`priority` must be none, low, medium or high, got `{other}`"),
    }
}

/// EventKit's 0–9 priority → the word the model reads (`None` for unset).
const fn priority_name(p: u8) -> Option<&'static str> {
    match p {
        0 => None,
        1..=4 => Some("high"),
        5 => Some("medium"),
        _ => Some("low"),
    }
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
        "priority": priority_name(r.priority),
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
/// `get_current_datetime` tool to resolve relative dates itself — one honest format
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
        "`{s}` isn't a due date I can read — use \"YYYY-MM-DD\" or \"YYYY-MM-DD HH:MM\" (resolve relative dates with the get_current_datetime tool first)"
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
            assert!(err.contains("get_current_datetime"), "{bad}: {err}");
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
        for bad in ["delete", "remove", "delete_list", "rename_list", "export", "share", ""] {
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
            priority: 1,
        };
        let v = render(&r);
        assert_eq!(v["id"], "abc-123");
        assert_eq!(v["due"], "2026-08-05 09:30");
        assert_eq!(v["notes"], Value::Null);
        assert_eq!(v["completed"], false);
        assert_eq!(v["priority"], "high");
    }

    #[test]
    fn lists_takes_no_arguments() {
        assert_eq!(Call::parse(&json!({"verb": "lists"})).unwrap(), Call::Lists);
    }

    #[test]
    fn create_list_needs_a_name_and_takes_an_optional_sibling() {
        let err = Call::parse(&json!({"verb": "create_list"})).unwrap_err().to_string();
        assert!(err.contains("needs the new list's `name`"), "{err}");
        assert!(Call::parse(&json!({"verb": "create_list", "name": "  "})).is_err(), "a blank name is missing");
        assert_eq!(
            Call::parse(&json!({"verb": "create_list", "name": "Cleaning", "like": "House"})).unwrap(),
            Call::CreateList {
                name: "Cleaning".to_owned(),
                like: Some("House".to_owned())
            }
        );
        assert_eq!(
            Call::parse(&json!({"verb": "create_list", "name": "Outdoor"})).unwrap(),
            Call::CreateList {
                name: "Outdoor".to_owned(),
                like: None
            }
        );
    }

    #[test]
    fn move_needs_an_id_and_a_destination_list() {
        let err = Call::parse(&json!({"verb": "move", "list": "Cleaning"})).unwrap_err().to_string();
        assert!(err.contains("run a `list` first"), "{err}");
        let err = Call::parse(&json!({"verb": "move", "id": "r-1"})).unwrap_err().to_string();
        assert!(err.contains("destination `list`"), "{err}");
        assert_eq!(
            Call::parse(&json!({"verb": "move", "id": "r-1", "list": "Paint Repairs"})).unwrap(),
            Call::Move {
                id: "r-1".to_owned(),
                list: "Paint Repairs".to_owned()
            }
        );
    }

    #[test]
    fn update_needs_an_id_and_at_least_one_change() {
        assert!(Call::parse(&json!({"verb": "update", "title": "x"})).is_err(), "no id");
        let err = Call::parse(&json!({"verb": "update", "id": "r-1"})).unwrap_err().to_string();
        assert!(err.contains("at least one of"), "{err}");
        let err = Call::parse(&json!({"verb": "update", "id": "r-1", "title": "  "})).unwrap_err().to_string();
        assert!(err.contains("can't be blank"), "{err}");
    }

    #[test]
    fn update_leaves_absent_fields_alone_and_clears_empty_ones() {
        let Call::Update { id, changes } = Call::parse(&json!({"verb": "update", "id": "r-1", "title": "Patch hallway", "notes": "", "due": ""})).unwrap()
        else {
            panic!("expected an update")
        };
        assert_eq!(id, "r-1");
        assert_eq!(
            changes,
            Changes {
                title: Some("Patch hallway".to_owned()),
                notes: Some(None),
                due: Some(None),
                priority: None,
            }
        );

        let Call::Update { changes, .. } =
            Call::parse(&json!({"verb": "update", "id": "r-1", "notes": "use the eggshell", "due": "2026-10-10 08:00", "priority": "High"})).unwrap()
        else {
            panic!("expected an update")
        };
        assert_eq!(changes.title, None, "an absent title is left alone");
        assert_eq!(changes.notes, Some(Some("use the eggshell".to_owned())));
        assert_eq!(changes.due.unwrap().unwrap().time, Some((8, 0)));
        assert_eq!(changes.priority, Some(1));
    }

    #[test]
    fn update_refuses_a_bad_due_date_or_priority() {
        let err = Call::parse(&json!({"verb": "update", "id": "r-1", "due": "tomorrow"})).unwrap_err().to_string();
        assert!(err.contains("get_current_datetime"), "{err}");
        let err = Call::parse(&json!({"verb": "update", "id": "r-1", "priority": "urgent"}))
            .unwrap_err()
            .to_string();
        assert!(err.contains("none, low, medium or high"), "{err}");
    }

    #[test]
    fn priority_words_round_trip_through_eventkit_values() {
        for word in ["low", "medium", "high"] {
            assert_eq!(priority_name(parse_priority(word).unwrap()), Some(word));
        }
        assert_eq!(priority_name(parse_priority("none").unwrap()), None);
        // Values other clients write still read as the right band.
        assert_eq!(priority_name(3), Some("high"));
        assert_eq!(priority_name(7), Some("low"));
    }

    #[test]
    fn the_description_says_what_is_and_is_not_supported() {
        // SMOODEV-3734: the model told the user it couldn't edit reminders and
        // then made 10 failing calls. The description must list the verbs it
        // has and point at the fallback for the ones it doesn't.
        let d = RemindersTool.schema().description;
        for verb in VERBS {
            assert!(d.contains(verb), "description must mention `{verb}`");
        }
        assert!(d.contains("NOT supported"), "{d}");
        assert!(d.contains("reminders_delete"), "{d}");
        assert!(d.contains("osascript"), "{d}");
    }

    #[test]
    fn delete_is_its_own_tool_and_requires_an_id() {
        let s = RemindersDeleteTool.schema();
        assert_eq!(s.name, "reminders_delete");
        assert_eq!(s.parameters["required"][0], "id");
        assert!(s.description.contains("confirm"), "{}", s.description);
        assert!(!RemindersDeleteTool.is_concurrent_safe());
        // The confirm gate matches tool names by substring; the everyday tool's
        // name must not contain the delete tool's, or every list would park.
        assert!(!"reminders".contains("reminders_delete"));
    }

    #[tokio::test]
    async fn delete_without_an_id_fails_before_touching_eventkit() {
        let err = RemindersDeleteTool.execute(json!({})).await.unwrap_err().to_string();
        assert!(err.contains("needs the `id`"), "{err}");
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

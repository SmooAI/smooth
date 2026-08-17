//! `imessage` — read, search and send the user's macOS Messages (pearl th-1665ed).
//!
//! ## Why this is a first-class tool and not `bash sqlite3 chat.db`
//!
//! **Both halves must run OUTSIDE the kernel sandbox**, for two different
//! reasons, so this follows the trusted-integration exception the `calendar`
//! tool established (see [`crate::calendar`] and `docs/Architecture/Security-Model.md`):
//!
//! - **Read** — `~/Library/Messages/chat.db` is Full-Disk-Access territory, and
//!   [`crate::sandbox`]'s profile denies reads under `~/Library` anyway. The read
//!   here is **in-process** `rusqlite` against a **read-only** connection, so it
//!   never builds a subprocess at all: no shell, no injection surface, and the
//!   TCC grant that matters is the one on the daemon's own app bundle.
//! - **Send** — there is no official Messages API. Sending is AppleScript
//!   automation (`osascript` → Messages.app over Apple Events), which needs
//!   `tccd`/mach lookups the seatbelt profile denies. So it spawns a plain
//!   [`tokio::process::Command`], exactly like `calendar` spawns `ical`.
//!
//! What keeps that honest:
//! - **argv only, no shell** — the recipient and body are passed to the script as
//!   `on run argv` arguments, never interpolated into AppleScript source. A
//!   recipient containing `"` or `\` cannot break out and run other AppleScript.
//! - **fixed binary** — `/usr/bin/osascript`, never caller-supplied.
//! - **fixed script** — one embedded [`SEND_SCRIPT`]; the caller supplies data,
//!   never code.
//! - **verb allowlist** ([`COMMANDS`]) — four reads plus `send`.
//! - **read-only SQL** — the connection carries `SQLITE_OPEN_READ_ONLY`, and every
//!   statement is a fixed `SELECT` with bound parameters.
//! - **still Narc-visible** — a normal tool call, so the daemon's permission gate
//!   and the Narc hook (secret redaction on results) see it like any other.
//!
//! ## Privacy posture — read this before widening anything
//!
//! This tool hands the model the user's private message history. Brent opted in
//! deliberately (pearl th-1665ed). The safeguards that keep it bounded:
//!
//! - Every read is **limited** — default [`DEFAULT_LIMIT`], hard cap [`MAX_LIMIT`].
//!   There is no "dump the whole database" shape.
//! - `thread` and `search` **require a filter**; only `recent`/`conversations` are
//!   unfiltered, and both are limited.
//! - Each message body is truncated at [`TEXT_CAP`] and the whole reply at
//!   [`OUTPUT_CAP`], so one pathological message can't flood the turn.
//! - Attachments are reported as a boolean, never as a filesystem path — the model
//!   gets no handle to go read the file.
//!
//! ## Availability
//! macOS-only (cfg-gated at registration). The tool registers even when it can't
//! work yet: a missing chat.db or an ungranted Full Disk Access returns actionable
//! setup guidance ("run `th doctor --setup-imessage`") instead of an opaque
//! failure, because that's something the agent can relay and the user can act on.

#![cfg(target_os = "macos")]

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{Local, TimeZone};
use rusqlite::types::Value as SqlValue;
use rusqlite::{Connection, OpenFlags};
use serde_json::{json, Value};
use smooth_menubar::setup::{initiate, Grant};
use smooth_operator::{Tool, ToolSchema};

/// Max bytes of output returned before truncation.
const OUTPUT_CAP: usize = 50_000;

/// Max characters of any single message body. A message can carry a pasted wall
/// of text; without this one row could eat the whole context window.
const TEXT_CAP: usize = 2_000;

/// Rows returned when the caller doesn't say.
const DEFAULT_LIMIT: i64 = 20;

/// Hard ceiling on rows, whatever the caller asks for. The privacy floor: there
/// is no shape of this tool that dumps the whole history in one call.
const MAX_LIMIT: i64 = 200;

/// Max characters in an outgoing message. Well past a real SMS/iMessage, short
/// enough that a runaway generation can't send a novel.
const MAX_SEND_CHARS: usize = 2_000;

/// Hard cap on the `osascript` send. Messages.app can block indefinitely when
/// the Automation grant is pending; a stuck child would stall the agent turn.
const SEND_TIMEOUT: Duration = Duration::from_secs(30);

/// Seconds between the Unix epoch and the Apple/Core Data epoch (2001-01-01).
const APPLE_EPOCH_OFFSET: i64 = 978_307_200;

/// The commands this tool exposes. An allowlist, not a denylist.
const COMMANDS: &[&str] = &["recent", "thread", "search", "conversations", "send"];

/// The setup instruction handed back whenever the integration isn't usable. One
/// string so the agent always relays the same next step.
const SETUP_HINT: &str =
    "Messages isn't set up yet — run `th doctor --setup-imessage` on the Mac (grants Full Disk Access so chat.db is readable, and primes the Messages automation permission), then try again.";

/// The AppleScript that sends. **Fixed** — the caller supplies `argv`, never
/// script text, which is what makes interpolation-injection impossible.
///
/// `participant … of targetService` is the iMessage path. Green-bubble SMS relay
/// through a paired iPhone is deliberately not attempted: it needs a different
/// service lookup and fails differently on every macOS release.
const SEND_SCRIPT: &str = r#"on run argv
    set recipientId to item 1 of argv
    set messageText to item 2 of argv
    tell application "Messages"
        set targetService to 1st account whose service type = iMessage
        send messageText to participant recipientId of targetService
    end tell
end run"#;

/// `imessage` — read, search and send macOS Messages.
pub struct IMessageTool;

/// Path to the Messages database: `SMOOTH_CHAT_DB` (tests + odd setups) →
/// `~/Library/Messages/chat.db`.
#[must_use]
pub fn chat_db_path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("SMOOTH_CHAT_DB").map(PathBuf::from) {
        return Some(p);
    }
    dirs_next::home_dir().map(|h| h.join("Library").join("Messages").join("chat.db"))
}

/// Why a chat.db read can't happen, as an actionable answer rather than an error.
#[derive(Debug, PartialEq, Eq)]
pub enum Unavailable {
    /// No file at the expected path — Messages has never run, or the path is odd.
    Missing,
    /// The file is there but this process can't read it: Full Disk Access.
    Denied,
}

impl Unavailable {
    fn message(&self, path: &Path) -> String {
        match self {
            Self::Missing => format!(
                "No Messages database at {} — Messages.app may never have been used on this Mac. {SETUP_HINT}",
                path.display()
            ),
            // Full Disk Access has no prompt API, so the most this process can
            // do is put the pane in front of the user — once (pearl th-ba764e).
            Self::Denied => format!(
                "Big Smooth can't read the Messages database at {} — macOS Full Disk Access has not been granted. {}",
                path.display(),
                initiate(Grant::FullDiskAccess).unwrap_or(SETUP_HINT)
            ),
        }
    }
}

/// Classify a chat.db path before trying to open it, so the "not set up yet"
/// answer is a plain sentence instead of a SQLite error.
///
/// # Errors
/// Never — the `Result` shape is the caller's convenience; `Ok(())` means readable.
pub fn probe(path: &Path) -> Result<(), Unavailable> {
    if !path.exists() {
        return Err(Unavailable::Missing);
    }
    // An actual byte read is the only honest FDA probe: metadata succeeds on a
    // TCC-denied file, so `exists()` alone would report a false ready.
    match std::fs::File::open(path) {
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => Err(Unavailable::Denied),
        // Anything else (a weird I/O error) — let the SQLite open produce the
        // real message rather than guessing here.
        Err(_) => Ok(()),
    }
}

#[async_trait]
impl Tool for IMessageTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "imessage".into(),
            description: format!(
                "Read, search and SEND the user's real macOS Messages (iMessage/SMS). Use it for anything about their texts — what did someone say, find a conversation, catch up on what was missed — and to text someone. Commands: {}. Reads: {{\"command\":\"recent\"}} (latest messages across every chat), {{\"command\":\"thread\",\"contact\":\"Mom\"}} (one conversation, newest last), {{\"command\":\"search\",\"query\":\"dinner\"}}, {{\"command\":\"conversations\"}} (who they talk to, most recent first). Send: {{\"command\":\"send\",\"contact\":\"+15551234567\",\"text\":\"on my way\"}} — `contact` must be an exact phone number or email (run `conversations` or `thread` first to get it); a nickname will NOT resolve. These are the user's PRIVATE messages: read only what the question needs, and never repeat message contents into anything that leaves this conversation. Output is JSON.",
                COMMANDS.join(", ")
            ),
            parameters: json!({
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "enum": COMMANDS,
                        "description": "recent (latest messages everywhere), thread (one conversation with a contact), search (find messages by text), conversations (list chats, most recently active first), send (send an iMessage)."
                    },
                    "contact": {
                        "type": "string",
                        "description": "Who. For `thread`: a phone number, email, or part of a contact/group name — matched loosely. For `send`: the EXACT phone number (+15551234567) or Apple ID email of the recipient; loose names do not resolve when sending."
                    },
                    "query": {
                        "type": "string",
                        "description": "Text to search for, for `search`. Matched case-insensitively anywhere in the message body."
                    },
                    "text": {
                        "type": "string",
                        "description": format!("The message body to send, for `send`. Max {MAX_SEND_CHARS} characters.")
                    },
                    "limit": {
                        "type": "integer",
                        "description": format!("How many rows to return (default {DEFAULT_LIMIT}, max {MAX_LIMIT}).")
                    }
                },
                "required": ["command"]
            }),
        }
    }

    fn is_concurrent_safe(&self) -> bool {
        // ponytail: the flag is per-tool, not per-call, and `send` is a real-world
        // side effect — so the whole tool serializes. Split into `imessage` +
        // `imessage_send` if parallel reads ever actually matter.
        false
    }

    async fn execute(&self, arguments: Value) -> anyhow::Result<String> {
        let command = command_of(&arguments)?;
        if command == "send" {
            let (contact, text) = send_args(&arguments)?;
            return send_message(&contact, &text).await;
        }

        let Some(path) = chat_db_path() else {
            return Ok(format!(
                "Cannot determine the home directory, so the Messages database can't be located. {SETUP_HINT}"
            ));
        };
        if let Err(why) = probe(&path) {
            return Ok(why.message(&path));
        }
        let query = build_query(command, &arguments)?;
        // rusqlite is synchronous; keep the reactor free while SQLite works.
        let rows = tokio::task::spawn_blocking(move || run_query(&path, &query)).await??;
        Ok(truncate(&serde_json::to_string_pretty(&rows)?))
    }
}

/// Pull and validate `command` against the allowlist.
fn command_of(arguments: &Value) -> anyhow::Result<&'static str> {
    let raw = arguments
        .get("command")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|c| !c.is_empty())
        .ok_or_else(|| anyhow::anyhow!("missing required string parameter `command`"))?;
    COMMANDS
        .iter()
        .find(|c| **c == raw)
        .copied()
        .ok_or_else(|| anyhow::anyhow!("`{raw}` is not an allowed imessage command. Allowed: {}", COMMANDS.join(", ")))
}

/// A read to run: fixed SQL plus bound parameters. Built separately from
/// execution so the shape of every query is unit-testable without a database.
#[derive(Debug, PartialEq)]
pub struct ReadQuery {
    sql: String,
    params: Vec<SqlValue>,
    /// Conversation rows have a different shape from message rows.
    conversations: bool,
    /// `thread` reads newest-first for the LIMIT, then flips so the model sees
    /// the conversation in the order it happened.
    chronological: bool,
}

/// The message columns every message-shaped read selects, in the order
/// [`message_row`] unpacks them.
const MESSAGE_COLUMNS: &str =
    "m.ROWID, m.date, m.is_from_me, m.text, m.attributedBody, m.cache_has_attachments, m.service, h.id, c.display_name, c.chat_identifier";

/// The joins every message-shaped read needs. `GROUP BY m.ROWID` collapses the
/// duplicate rows a message that belongs to more than one chat would otherwise
/// produce through `chat_message_join`.
const MESSAGE_FROM: &str = "FROM message m
     LEFT JOIN handle h ON m.handle_id = h.ROWID
     LEFT JOIN chat_message_join cmj ON cmj.message_id = m.ROWID
     LEFT JOIN chat c ON c.ROWID = cmj.chat_id";

/// Build the SQL for a read command. Every value the caller supplies is a bound
/// parameter — no string is ever concatenated into the statement.
///
/// # Errors
/// When a command's required filter (`contact` / `query`) is missing or blank.
pub fn build_query(command: &str, arguments: &Value) -> anyhow::Result<ReadQuery> {
    let limit = limit_of(arguments);
    match command {
        "recent" => Ok(ReadQuery {
            sql: format!("SELECT {MESSAGE_COLUMNS} {MESSAGE_FROM} GROUP BY m.ROWID ORDER BY m.date DESC LIMIT ?1"),
            params: vec![SqlValue::Integer(limit)],
            conversations: false,
            chronological: false,
        }),
        "thread" => {
            let contact = required_str(
                arguments,
                "contact",
                "`thread` needs `contact` — who the conversation is with (a phone number, email, or part of a name)",
            )?;
            Ok(ReadQuery {
                // Loose match across the three places a person can be named: the
                // handle (phone/email), the chat id, and a group's display name.
                // ponytail: no phone-number normalization — "+1 555" vs "5551234"
                // won't cross-match. `conversations` gives the model the exact
                // handle to use, which is the reliable path.
                sql: format!(
                    "SELECT {MESSAGE_COLUMNS} {MESSAGE_FROM}
                     WHERE h.id LIKE ?1 OR c.chat_identifier LIKE ?1 OR c.display_name LIKE ?1
                     GROUP BY m.ROWID ORDER BY m.date DESC LIMIT ?2"
                ),
                params: vec![SqlValue::Text(contains(&contact)), SqlValue::Integer(limit)],
                conversations: false,
                chronological: true,
            })
        }
        "search" => {
            let query = required_str(arguments, "query", "`search` needs `query` — the text to look for")?;
            Ok(ReadQuery {
                // Messages composed on modern macOS often have a NULL `text` and
                // carry the body only in the `attributedBody` typedstream blob.
                // ponytail: CAST(blob AS TEXT) LIKE is a coarse match against the
                // blob's mostly-UTF-8 payload — it can miss a term split by the
                // archive's binary framing. Good enough to find the message; the
                // body the caller reads back is the properly decoded one.
                sql: format!(
                    "SELECT {MESSAGE_COLUMNS} {MESSAGE_FROM}
                     WHERE m.text LIKE ?1 OR (m.text IS NULL AND CAST(m.attributedBody AS TEXT) LIKE ?1)
                     GROUP BY m.ROWID ORDER BY m.date DESC LIMIT ?2"
                ),
                params: vec![SqlValue::Text(contains(&query)), SqlValue::Integer(limit)],
                conversations: false,
                chronological: false,
            })
        }
        "conversations" => Ok(ReadQuery {
            sql: "SELECT c.chat_identifier, c.display_name, c.service_name, MAX(m.date), COUNT(m.ROWID)
                  FROM chat c
                  JOIN chat_message_join cmj ON cmj.chat_id = c.ROWID
                  JOIN message m ON m.ROWID = cmj.message_id
                  GROUP BY c.ROWID ORDER BY MAX(m.date) DESC LIMIT ?1"
                .to_owned(),
            params: vec![SqlValue::Integer(limit)],
            conversations: true,
            chronological: false,
        }),
        other => anyhow::bail!("`{other}` is not a readable imessage command"),
    }
}

/// Clamp the caller's `limit` into `1..=MAX_LIMIT`. A missing, non-numeric, zero
/// or negative value falls back to the default rather than erroring — the model
/// asking for "0 messages" means it didn't mean to ask at all.
fn limit_of(arguments: &Value) -> i64 {
    arguments
        .get("limit")
        .and_then(Value::as_i64)
        .filter(|n| *n > 0)
        .unwrap_or(DEFAULT_LIMIT)
        .min(MAX_LIMIT)
}

fn required_str(arguments: &Value, key: &str, msg: &str) -> anyhow::Result<String> {
    arguments
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(|| anyhow::anyhow!("{msg}"))
}

/// A `LIKE` "contains" pattern. `%` and `_` in the needle stay literal-ish
/// (over-matching at worst) — it's a bound parameter, so there is no injection
/// concern, only a precision one.
fn contains(needle: &str) -> String {
    format!("%{needle}%")
}

/// Open chat.db **read-only** and run `query`.
fn run_query(path: &Path, query: &ReadQuery) -> anyhow::Result<Vec<Value>> {
    let conn = open_read_only(path)?;
    let mut stmt = conn.prepare(&query.sql)?;
    let params = rusqlite::params_from_iter(query.params.iter());
    let mut rows = stmt.query(params)?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        out.push(if query.conversations { conversation_row(row)? } else { message_row(row)? });
    }
    if query.chronological {
        out.reverse();
    }
    Ok(out)
}

/// A read-only connection to chat.db.
///
/// chat.db is a WAL database, and SQLite needs to write the `-shm` index to read
/// a WAL db normally — which a read-only open can't do while Messages.app holds
/// it. The fallback is the `immutable=1` URI: it tells SQLite the file won't
/// change, skipping the WAL machinery entirely. That can miss messages still
/// sitting in an uncheckpointed WAL, which is the right trade — stale by seconds
/// beats failing outright.
fn open_read_only(path: &Path) -> anyhow::Result<Connection> {
    match Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY) {
        Ok(conn) => Ok(conn),
        Err(first) => {
            let uri = format!("file:{}?immutable=1", path.display());
            Connection::open_with_flags(uri, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI).map_err(|second| {
                anyhow::anyhow!(
                    "cannot open the Messages database at {} ({first}; immutable retry: {second}). {SETUP_HINT}",
                    path.display()
                )
            })
        }
    }
}

/// One message row → JSON, in [`MESSAGE_COLUMNS`] order.
fn message_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Value> {
    let id: i64 = row.get(0)?;
    let date: i64 = row.get(1).unwrap_or(0);
    let is_from_me: i64 = row.get(2).unwrap_or(0);
    let text: Option<String> = row.get(3).unwrap_or(None);
    let blob: Option<Vec<u8>> = row.get(4).unwrap_or(None);
    let has_attachments: i64 = row.get(5).unwrap_or(0);
    let service: Option<String> = row.get(6).unwrap_or(None);
    let handle: Option<String> = row.get(7).unwrap_or(None);
    let display_name: Option<String> = row.get(8).unwrap_or(None);
    let chat_identifier: Option<String> = row.get(9).unwrap_or(None);

    let body = text
        .filter(|t| !t.is_empty())
        .or_else(|| blob.as_deref().and_then(extract_attributed_body))
        .map(|t| cap_chars(&t, TEXT_CAP));

    Ok(json!({
        "id": id,
        "at": format_apple_date(date),
        "from": if is_from_me == 1 { "me".to_owned() } else { handle.clone().unwrap_or_else(|| "unknown".to_owned()) },
        "handle": handle,
        "chat": display_name.filter(|d| !d.is_empty()).or(chat_identifier),
        "service": service,
        "text": body,
        // Deliberately a boolean, not a path: the model gets no filesystem handle
        // to go read the user's photos with.
        "has_attachments": has_attachments == 1,
    }))
}

/// One conversation row → JSON.
fn conversation_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Value> {
    let chat_identifier: Option<String> = row.get(0).unwrap_or(None);
    let display_name: Option<String> = row.get(1).unwrap_or(None);
    let service: Option<String> = row.get(2).unwrap_or(None);
    let last: i64 = row.get(3).unwrap_or(0);
    let count: i64 = row.get(4).unwrap_or(0);
    Ok(json!({
        "chat": chat_identifier,
        "name": display_name.filter(|d| !d.is_empty()),
        "service": service,
        "last_message_at": format_apple_date(last),
        "message_count": count,
    }))
}

/// Apple/Core Data timestamp → local ISO-8601.
///
/// The column is nanoseconds since 2001-01-01 on macOS 10.13+, and *seconds*
/// before that — a magnitude test tells them apart (a plausible second-count is
/// under ~10^10; a nanosecond-count is over ~10^17).
fn format_apple_date(raw: i64) -> Option<String> {
    if raw == 0 {
        return None;
    }
    let seconds = if raw.abs() > 100_000_000_000 { raw / 1_000_000_000 } else { raw };
    Local
        .timestamp_opt(seconds + APPLE_EPOCH_OFFSET, 0)
        .single()
        .map(|dt| dt.to_rfc3339_opts(chrono::SecondsFormat::Secs, false))
}

/// Pull the message body out of an `attributedBody` NSAttributedString
/// typedstream archive.
///
/// Messages composed on modern macOS leave `message.text` NULL and put the body
/// here. A full typedstream decoder is a large dependency for one field, so this
/// is the focused read every chat.db reader converges on: find the `NSString`
/// class marker, step over the archive's four framing bytes plus the string's
/// encoding marker, then read a length-prefixed UTF-8 run.
///
/// The prefix is a variable-width count: `< 0x80` is a one-byte length, `0x81`
/// introduces a little-endian `u16`, `0x82` a little-endian `u32`.
///
/// ponytail: a heuristic on a private format, so it is written to *fail to None*
/// on anything unexpected rather than to be exhaustively correct — the caller
/// falls back to no body, never to garbage.
#[must_use]
pub fn extract_attributed_body(blob: &[u8]) -> Option<String> {
    const MARKER: &[u8] = b"NSString";
    let start = blob.windows(MARKER.len()).position(|w| w == MARKER)? + MARKER.len();
    // The four bytes after the class name are typedstream framing, then the
    // string's own encoding marker, then the length.
    let rest = blob.get(start.checked_add(5)?..)?;
    let (len, body) = match *rest.first()? {
        0x81 => (u16::from_le_bytes([*rest.get(1)?, *rest.get(2)?]) as usize, rest.get(3..)?),
        0x82 => (
            u32::from_le_bytes([*rest.get(1)?, *rest.get(2)?, *rest.get(3)?, *rest.get(4)?]) as usize,
            rest.get(5..)?,
        ),
        // 0x83+ would be a u64 count — a >4GB message does not exist.
        n if n < 0x80 => (n as usize, rest.get(1..)?),
        _ => return None,
    };
    let raw = body.get(..len)?;
    // Lossy, not strict: a body that ends mid-multibyte (truncated archive)
    // should degrade to replacement chars, not vanish.
    let text = String::from_utf8_lossy(raw).into_owned();
    (!text.trim().is_empty()).then_some(text)
}

/// Validate the `send` arguments.
fn send_args(arguments: &Value) -> anyhow::Result<(String, String)> {
    let contact = required_str(
        arguments,
        "contact",
        "`send` needs `contact` — the exact phone number (+15551234567) or Apple ID email to send to. Run `conversations` or `thread` first to get it; a nickname will not resolve.",
    )?;
    let text = required_str(arguments, "text", "`send` needs `text` — the message body to send")?;
    if text.chars().count() > MAX_SEND_CHARS {
        anyhow::bail!("message is {} characters; the limit is {MAX_SEND_CHARS}", text.chars().count());
    }
    Ok((contact, text))
}

/// Send via Messages.app, **outside** the kernel sandbox (see the module docs).
///
/// The recipient and body go over as `argv`, so no caller-supplied byte is ever
/// parsed as AppleScript.
async fn send_message(contact: &str, text: &str) -> anyhow::Result<String> {
    let mut cmd = tokio::process::Command::new("/usr/bin/osascript");
    cmd.arg("-e")
        .arg(SEND_SCRIPT)
        .arg(contact)
        .arg(text)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let child = cmd.spawn().map_err(|e| anyhow::anyhow!("failed to spawn `osascript`: {e}"))?;
    let output = match tokio::time::timeout(SEND_TIMEOUT, child.wait_with_output()).await {
        Ok(r) => r.map_err(|e| anyhow::anyhow!("`osascript` error: {e}"))?,
        Err(_) => anyhow::bail!(
            "sending timed out after {}s — Messages.app may be waiting on a permission prompt. {SETUP_HINT}",
            SEND_TIMEOUT.as_secs()
        ),
    };
    if output.status.success() {
        return Ok(json!({"sent": true, "to": contact, "text": text}).to_string());
    }

    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    if looks_like_automation_denial(&stderr) {
        // Re-fire the Apple Event as a no-op probe: on a never-answered grant
        // that's what makes the prompt appear (pearl th-ba764e).
        let next_step = initiate(Grant::MessagesAutomation).unwrap_or(SETUP_HINT);
        return Ok(format!(
            "Big Smooth isn't allowed to control Messages.app. {next_step}\n\n--- osascript said ---\n{}",
            truncate(&stderr)
        ));
    }
    Ok(format!("Sending to {contact} failed.\n{}", truncate(&stderr)))
}

/// Whether `text` reads like a TCC Automation denial (or a recipient Messages
/// couldn't resolve) rather than an unrelated AppleScript error. The wording
/// varies by macOS release, so this matches the vocabulary they share.
fn looks_like_automation_denial(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    [
        "not authorized",
        "not allowed",
        "permission",
        "-1743", // errAEEventNotPermitted — the Automation TCC denial
        "access for assistive",
        "can't get participant",
        "invalid index",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

/// Truncate to `max` **characters**, never splitting a multibyte char.
fn cap_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_owned();
    }
    let kept: String = s.chars().take(max).collect();
    format!("{kept}… [truncated]")
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
#[allow(clippy::unwrap_used, clippy::expect_used, reason = "unwrap/expect are the idiom for test assertions")]
mod tests {
    use super::*;

    // ---- schema / argument validation -------------------------------------

    #[test]
    fn schema_names_imessage_and_requires_command() {
        let s = IMessageTool.schema();
        assert_eq!(s.name, "imessage");
        assert_eq!(s.parameters["required"][0], "command");
        assert!(!IMessageTool.is_concurrent_safe(), "send is a real-world side effect, so the tool serializes");
        // The description must warn the model these are private — the safeguard
        // that no code path can enforce.
        assert!(s.schema_description_mentions_privacy(), "{}", s.description);
    }

    /// Small helper so the assertion above reads as intent.
    trait PrivacyDescription {
        fn schema_description_mentions_privacy(&self) -> bool;
    }
    impl PrivacyDescription for ToolSchema {
        fn schema_description_mentions_privacy(&self) -> bool {
            self.description.contains("PRIVATE")
        }
    }

    #[test]
    fn command_allowlist_refuses_anything_else() {
        for bad in ["delete", "export", "attachments", "drop", "read", ""] {
            assert!(command_of(&json!({"command": bad})).is_err(), "{bad} must be refused");
        }
        for good in COMMANDS {
            assert_eq!(command_of(&json!({"command": good})).unwrap(), *good);
        }
    }

    #[test]
    fn command_rejects_missing_blank_and_non_string() {
        assert!(command_of(&json!({})).is_err());
        assert!(command_of(&json!({"command": "   "})).is_err());
        assert!(command_of(&json!({"command": 3})).is_err());
    }

    #[test]
    fn thread_and_search_require_their_filter() {
        // The privacy floor: neither unfiltered shape is reachable.
        let err = build_query("thread", &json!({})).unwrap_err().to_string();
        assert!(err.contains("needs `contact`"), "{err}");
        let err = build_query("search", &json!({"query": "  "})).unwrap_err().to_string();
        assert!(err.contains("needs `query`"), "{err}");
    }

    #[test]
    fn limit_defaults_and_is_capped() {
        assert_eq!(limit_of(&json!({})), DEFAULT_LIMIT);
        assert_eq!(limit_of(&json!({"limit": 5})), 5);
        assert_eq!(limit_of(&json!({"limit": 10_000})), MAX_LIMIT, "the hard cap is the privacy floor");
        // Nonsense falls back rather than erroring or returning everything.
        assert_eq!(limit_of(&json!({"limit": 0})), DEFAULT_LIMIT);
        assert_eq!(limit_of(&json!({"limit": -7})), DEFAULT_LIMIT);
        assert_eq!(limit_of(&json!({"limit": "lots"})), DEFAULT_LIMIT);
    }

    #[test]
    fn every_read_query_carries_a_limit() {
        for (cmd, args) in [
            ("recent", json!({"limit": 9999})),
            ("thread", json!({"contact": "x", "limit": 9999})),
            ("search", json!({"query": "x", "limit": 9999})),
            ("conversations", json!({"limit": 9999})),
        ] {
            let q = build_query(cmd, &args).unwrap();
            assert!(q.sql.contains("LIMIT"), "{cmd} must be bounded: {}", q.sql);
            assert!(q.params.contains(&SqlValue::Integer(MAX_LIMIT)), "{cmd} must clamp to the cap: {:?}", q.params);
        }
    }

    #[test]
    fn read_queries_are_selects_with_bound_parameters_only() {
        for (cmd, args) in [
            ("recent", json!({})),
            ("thread", json!({"contact": "'; DROP TABLE message; --"})),
            ("search", json!({"query": "%' OR 1=1 --"})),
            ("conversations", json!({})),
        ] {
            let q = build_query(cmd, &args).unwrap();
            assert!(q.sql.trim_start().starts_with("SELECT"), "{cmd}: {}", q.sql);
            // The injection attempt lives in a parameter, never in the statement.
            assert!(!q.sql.contains("DROP"), "{cmd}: {}", q.sql);
            assert!(!q.sql.contains("OR 1=1"), "{cmd}: {}", q.sql);
        }
    }

    #[test]
    fn thread_reads_back_in_conversation_order() {
        let q = build_query("thread", &json!({"contact": "Mom"})).unwrap();
        assert!(q.chronological, "a thread must read oldest→newest");
        assert!(!build_query("recent", &json!({})).unwrap().chronological);
    }

    #[test]
    fn thread_matches_handle_chat_and_group_name() {
        let q = build_query("thread", &json!({"contact": "Mom"})).unwrap();
        assert!(q.sql.contains("h.id LIKE ?1"), "{}", q.sql);
        assert!(q.sql.contains("c.display_name LIKE ?1"), "{}", q.sql);
        assert_eq!(q.params[0], SqlValue::Text("%Mom%".into()));
    }

    #[test]
    fn search_also_looks_inside_attributed_bodies() {
        // Regression guard: modern macOS leaves `text` NULL, so a text-only
        // search silently misses a large share of the history.
        let q = build_query("search", &json!({"query": "dinner"})).unwrap();
        assert!(q.sql.contains("attributedBody"), "{}", q.sql);
    }

    #[test]
    fn build_query_refuses_send_and_unknown_commands() {
        assert!(build_query("send", &json!({})).is_err(), "send is not a read");
        assert!(build_query("nope", &json!({})).is_err());
    }

    // ---- date handling -----------------------------------------------------

    #[test]
    fn apple_nanosecond_and_second_timestamps_both_decode() {
        // 2023-01-01T00:00:00Z = 1672531200 unix = 694224000 Apple seconds.
        let apple_seconds = 694_224_000;
        let from_seconds = format_apple_date(apple_seconds).unwrap();
        let from_nanos = format_apple_date(apple_seconds * 1_000_000_000).unwrap();
        assert_eq!(from_seconds, from_nanos, "both encodings must land on the same instant");
        assert!(
            from_seconds.starts_with("2022-12-31") || from_seconds.starts_with("2023-01-01"),
            "{from_seconds}"
        );
    }

    #[test]
    fn a_zero_date_is_no_date_not_the_year_2001() {
        assert_eq!(format_apple_date(0), None);
    }

    // ---- attributedBody decoding ------------------------------------------

    /// A synthetic typedstream fragment: `NSString`, four framing bytes, the
    /// encoding marker, then a length-prefixed body.
    fn attributed(body: &str, prefix: &[u8]) -> Vec<u8> {
        let mut v = b"\x04\x0bstreamtyped\x81\xe8\x03\x84\x01\x40\x84\x84\x84".to_vec();
        v.extend_from_slice(b"NSString");
        v.extend_from_slice(&[0x01, 0x94, 0x84, 0x01, 0x2b]); // framing + '+'
        v.extend_from_slice(prefix);
        v.extend_from_slice(body.as_bytes());
        v.extend_from_slice(b"\x86\x84\x02iI"); // trailing archive junk
        v
    }

    #[test]
    fn short_body_uses_the_one_byte_length_prefix() {
        let blob = attributed("on my way", &[9]);
        assert_eq!(extract_attributed_body(&blob).as_deref(), Some("on my way"));
    }

    #[test]
    fn long_body_uses_the_0x81_u16_prefix() {
        let body = "x".repeat(300);
        #[allow(clippy::cast_possible_truncation, reason = "300 fits a u16 by construction")]
        let prefix = [0x81, (300u16 & 0xff) as u8, (300u16 >> 8) as u8];
        let blob = attributed(&body, &prefix);
        assert_eq!(extract_attributed_body(&blob).as_deref(), Some(body.as_str()));
    }

    #[test]
    fn very_long_body_uses_the_0x82_u32_prefix() {
        let body = "y".repeat(70_000);
        let mut prefix = vec![0x82];
        prefix.extend_from_slice(&70_000u32.to_le_bytes());
        let blob = attributed(&body, &prefix);
        assert_eq!(extract_attributed_body(&blob).as_deref(), Some(body.as_str()));
    }

    #[test]
    fn multibyte_bodies_survive_the_byte_length_prefix() {
        // The prefix counts BYTES, not chars — an emoji body catches an
        // accidental char-count.
        let body = "héllo 👋";
        #[allow(clippy::cast_possible_truncation, reason = "the fixture body is far under 255 bytes")]
        let prefix = [body.len() as u8];
        let blob = attributed(body, &prefix);
        assert_eq!(extract_attributed_body(&blob).as_deref(), Some(body));
    }

    #[test]
    fn malformed_blobs_degrade_to_none_never_to_garbage() {
        for bad in [
            &b""[..],
            &b"no marker here at all"[..],
            &b"NSString"[..],                                                      // marker, then nothing
            &b"NSString\x01\x94\x84\x01"[..],                                      // truncated before the length
            &b"NSString\x01\x94\x84\x01\x2b"[..],                                  // length byte missing
            &[b"NSString\x01\x94\x84\x01\x2b".as_slice(), &[0x81, 0x10]].concat(), // length runs past the end
            &[b"NSString\x01\x94\x84\x01\x2b".as_slice(), &[0x99]].concat(),       // reserved prefix
        ] {
            assert_eq!(extract_attributed_body(bad), None, "{bad:?} must decode to None");
        }
    }

    #[test]
    fn a_whitespace_only_body_is_not_a_body() {
        let blob = attributed("   ", &[3]);
        assert_eq!(extract_attributed_body(&blob), None);
    }

    // ---- send validation ---------------------------------------------------

    #[test]
    fn send_requires_a_recipient_and_a_body() {
        assert!(send_args(&json!({"text": "hi"})).is_err(), "no recipient");
        assert!(send_args(&json!({"contact": "+15551234567"})).is_err(), "no body");
        assert!(send_args(&json!({"contact": "  ", "text": "hi"})).is_err(), "blank recipient");
        let (to, body) = send_args(&json!({"contact": " +15551234567 ", "text": "hi"})).unwrap();
        assert_eq!((to.as_str(), body.as_str()), ("+15551234567", "hi"));
    }

    #[test]
    fn send_refuses_a_runaway_message() {
        let long = "z".repeat(MAX_SEND_CHARS + 1);
        let err = send_args(&json!({"contact": "+1", "text": long})).unwrap_err().to_string();
        assert!(err.contains("the limit is"), "{err}");
        // Exactly at the cap is fine.
        assert!(send_args(&json!({"contact": "+1", "text": "z".repeat(MAX_SEND_CHARS)})).is_ok());
    }

    #[test]
    fn the_send_script_is_fixed_and_reads_its_data_from_argv() {
        // The whole injection defence in one assertion: the script takes `argv`
        // and never interpolates. If someone ever reaches for format!() here,
        // this fails.
        assert!(SEND_SCRIPT.contains("on run argv"), "{SEND_SCRIPT}");
        assert!(SEND_SCRIPT.contains("item 1 of argv"));
        assert!(SEND_SCRIPT.contains("item 2 of argv"));
        assert!(!SEND_SCRIPT.contains("{}"), "no format placeholders may exist in the script");
    }

    #[tokio::test]
    async fn execute_validates_send_before_spawning_osascript() {
        // No recipient → refused at the argument gate, so Messages is never
        // touched. (A test that actually sent would text a real human.)
        let err = IMessageTool.execute(json!({"command": "send", "text": "hi"})).await.unwrap_err().to_string();
        assert!(err.contains("needs `contact`"), "{err}");
    }

    #[tokio::test]
    async fn execute_rejects_a_disallowed_command_before_touching_the_database() {
        let err = IMessageTool.execute(json!({"command": "delete_all"})).await.unwrap_err().to_string();
        assert!(err.contains("not an allowed imessage command"), "{err}");
    }

    // ---- availability ------------------------------------------------------

    #[test]
    fn probe_reports_a_missing_database_as_missing() {
        let missing = std::env::temp_dir().join("th-imessage-does-not-exist-xyz.db");
        assert_eq!(probe(&missing), Err(Unavailable::Missing));
        assert!(Unavailable::Missing.message(&missing).contains("th doctor --setup-imessage"));
        assert!(Unavailable::Denied.message(&missing).contains("Full Disk Access"));
    }

    #[test]
    fn probe_passes_a_readable_file() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("chat.db");
        std::fs::write(&f, b"").unwrap();
        assert_eq!(probe(&f), Ok(()));
    }

    /// The one test that mutates `SMOOTH_CHAT_DB`, so it also carries the full
    /// `execute()` round-trip — `Tool::execute` → path resolution → probe →
    /// query → JSON. Kept as a single test on purpose: two tests racing on the
    /// same process-global env var is a flake, and this is the only var here.
    #[tokio::test]
    async fn execute_reads_end_to_end_through_the_resolved_database_path() {
        let dir = tempfile::tempdir().unwrap();
        let db = fixture_db(dir.path());
        std::env::set_var("SMOOTH_CHAT_DB", &db);
        assert_eq!(chat_db_path().as_deref(), Some(db.as_path()));

        // A read the model would actually make, all the way through the Tool impl.
        let out = IMessageTool.execute(json!({"command": "thread", "contact": "Dinner"})).await.unwrap();
        let rows: Vec<Value> = serde_json::from_str(&out).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["text"], "bringing dessert", "the attributedBody decode must survive the full path");

        // An unreadable database answers with setup guidance, not a SQLite error.
        std::env::set_var("SMOOTH_CHAT_DB", dir.path().join("nope.db"));
        let out = IMessageTool.execute(json!({"command": "recent"})).await.unwrap();
        assert!(out.contains("th doctor --setup-imessage"), "{out}");

        std::env::remove_var("SMOOTH_CHAT_DB");
        assert!(chat_db_path().unwrap().ends_with("Library/Messages/chat.db"));
    }

    #[test]
    fn automation_denials_are_recognised_but_real_errors_are_not() {
        assert!(looks_like_automation_denial("execution error: Not authorized to send Apple events (-1743)"));
        assert!(looks_like_automation_denial("Messages got an error: Can't get participant \"x\""));
        assert!(!looks_like_automation_denial("syntax error: expected end of line"));
    }

    #[test]
    fn setup_hint_names_the_one_command_that_fixes_it() {
        assert!(SETUP_HINT.contains("th doctor --setup-imessage"));
    }

    // ---- caps --------------------------------------------------------------

    #[test]
    fn cap_chars_trims_on_a_char_boundary() {
        let long = "é".repeat(TEXT_CAP + 10);
        let out = cap_chars(&long, TEXT_CAP);
        assert!(out.ends_with("… [truncated]"));
        assert_eq!(out.chars().filter(|c| *c == 'é').count(), TEXT_CAP);
        assert_eq!(cap_chars("short", TEXT_CAP), "short");
    }

    #[test]
    fn truncate_caps_output_on_a_char_boundary() {
        let long = "é".repeat(OUTPUT_CAP);
        let out = truncate(&long);
        assert!(out.contains("truncated"));
        assert!(out.len() < long.len());
        assert_eq!(truncate("short"), "short");
    }

    // ---- end-to-end against a synthetic chat.db ----------------------------
    //
    // NEVER the real ~/Library/Messages/chat.db: these build a fixture with the
    // subset of the real schema the queries touch.

    /// Build a throwaway chat.db with the real column names and two chats.
    fn fixture_db(dir: &Path) -> PathBuf {
        let path = dir.join("chat.db");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE handle (ROWID INTEGER PRIMARY KEY, id TEXT, service TEXT);
             CREATE TABLE chat (ROWID INTEGER PRIMARY KEY, chat_identifier TEXT, display_name TEXT, service_name TEXT);
             CREATE TABLE message (ROWID INTEGER PRIMARY KEY, date INTEGER, is_from_me INTEGER, text TEXT,
                                   attributedBody BLOB, cache_has_attachments INTEGER, service TEXT, handle_id INTEGER);
             CREATE TABLE chat_message_join (chat_id INTEGER, message_id INTEGER);

             INSERT INTO handle VALUES (1, '+15551234567', 'iMessage'), (2, 'friend@example.com', 'iMessage');
             INSERT INTO chat VALUES (1, '+15551234567', '', 'iMessage'), (2, 'chat99', 'Dinner Crew', 'iMessage');",
        )
        .unwrap();

        // Apple-nanosecond timestamps, ascending.
        let base: i64 = 694_224_000_000_000_000;
        // (rowid, date, is_from_me, text, has_attach, handle, chat)
        #[allow(clippy::type_complexity)] // test fixture tuple, shape documented above
        let rows: &[(i64, i64, i64, Option<&str>, i64, i64, i64)] = &[
            (1, base, 0, Some("hey are we still on for dinner"), 0, 1, 1),
            (2, base + 60_000_000_000, 1, Some("yes! 7pm"), 0, 1, 1),
            (3, base + 120_000_000_000, 0, None, 1, 2, 2), // attributedBody-only + attachment
        ];
        for (id, date, from_me, text, attach, handle, chat) in rows {
            conn.execute(
                "INSERT INTO message (ROWID, date, is_from_me, text, attributedBody, cache_has_attachments, service, handle_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'iMessage', ?7)",
                rusqlite::params![
                    id,
                    date,
                    from_me,
                    text,
                    if text.is_none() { Some(attributed("bringing dessert", &[16])) } else { None },
                    attach,
                    handle
                ],
            )
            .unwrap();
            conn.execute("INSERT INTO chat_message_join VALUES (?1, ?2)", rusqlite::params![chat, id])
                .unwrap();
        }
        path
    }

    fn run(path: &Path, cmd: &str, args: Value) -> Vec<Value> {
        run_query(path, &build_query(cmd, &args).unwrap()).unwrap()
    }

    #[test]
    fn recent_returns_newest_first_with_sender_and_chat() {
        let dir = tempfile::tempdir().unwrap();
        let db = fixture_db(dir.path());
        let rows = run(&db, "recent", json!({}));
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0]["id"], 3, "newest first");
        assert_eq!(rows[0]["chat"], "Dinner Crew", "a group prefers its display name");
        assert_eq!(rows[0]["from"], "friend@example.com");
        assert_eq!(rows[1]["from"], "me", "is_from_me must not report the handle");
        assert_eq!(rows[2]["chat"], "+15551234567", "a 1:1 falls back to the chat identifier");
        assert!(rows[0]["at"].as_str().unwrap().starts_with("202"), "{}", rows[0]["at"]);
    }

    #[test]
    fn a_body_stored_only_as_attributedbody_still_reads() {
        // The whole reason the typedstream decoder exists.
        let dir = tempfile::tempdir().unwrap();
        let db = fixture_db(dir.path());
        let rows = run(&db, "recent", json!({"limit": 1}));
        assert_eq!(rows[0]["text"], "bringing dessert");
        assert_eq!(rows[0]["has_attachments"], true);
    }

    #[test]
    fn attachments_are_a_flag_never_a_filesystem_path() {
        let dir = tempfile::tempdir().unwrap();
        let db = fixture_db(dir.path());
        let rows = run(&db, "recent", json!({}));
        let dump = serde_json::to_string(&rows).unwrap();
        assert!(!dump.contains("/Library/Messages/Attachments"), "no attachment paths may leak: {dump}");
    }

    #[test]
    fn thread_filters_to_one_conversation_in_chronological_order() {
        let dir = tempfile::tempdir().unwrap();
        let db = fixture_db(dir.path());
        let rows = run(&db, "thread", json!({"contact": "5551234567"}));
        assert_eq!(rows.len(), 2, "only the 1:1 chat: {rows:?}");
        assert_eq!(rows[0]["id"], 1, "oldest first, so the model reads it as a conversation");
        assert_eq!(rows[1]["id"], 2);
        // A group is reachable by its display name too.
        let group = run(&db, "thread", json!({"contact": "Dinner"}));
        assert_eq!(group.len(), 1);
        assert_eq!(group[0]["id"], 3);
    }

    #[test]
    fn search_finds_plain_text_and_attributedbody_only_messages() {
        let dir = tempfile::tempdir().unwrap();
        let db = fixture_db(dir.path());
        assert_eq!(run(&db, "search", json!({"query": "dinner"})).len(), 1);
        // "dessert" exists only inside the typedstream blob.
        let blob_hit = run(&db, "search", json!({"query": "dessert"}));
        assert_eq!(blob_hit.len(), 1, "attributedBody-only messages must be findable");
        assert_eq!(blob_hit[0]["text"], "bringing dessert");
        assert!(run(&db, "search", json!({"query": "zzzznotpresent"})).is_empty());
    }

    #[test]
    fn a_sql_injection_attempt_in_a_filter_matches_nothing_and_breaks_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let db = fixture_db(dir.path());
        assert!(run(&db, "search", json!({"query": "'; DROP TABLE message; --"})).is_empty());
        // The table is still there afterwards.
        assert_eq!(run(&db, "recent", json!({})).len(), 3);
    }

    #[test]
    fn conversations_lists_chats_most_recent_first() {
        let dir = tempfile::tempdir().unwrap();
        let db = fixture_db(dir.path());
        let rows = run(&db, "conversations", json!({}));
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["name"], "Dinner Crew", "most recently active first");
        assert_eq!(rows[0]["message_count"], 1);
        assert_eq!(rows[1]["chat"], "+15551234567");
        assert_eq!(rows[1]["name"], Value::Null, "an empty display name is not a name");
        assert_eq!(rows[1]["message_count"], 2);
    }

    #[test]
    fn the_limit_actually_limits() {
        let dir = tempfile::tempdir().unwrap();
        let db = fixture_db(dir.path());
        assert_eq!(run(&db, "recent", json!({"limit": 1})).len(), 1);
        assert_eq!(run(&db, "recent", json!({"limit": 2})).len(), 2);
    }

    #[test]
    fn the_connection_is_read_only() {
        // The load-bearing claim of the read half: even if a query were ever
        // built wrong, the connection itself cannot mutate the user's messages.
        let dir = tempfile::tempdir().unwrap();
        let db = fixture_db(dir.path());
        let conn = open_read_only(&db).unwrap();
        let err = conn.execute("DELETE FROM message", []).unwrap_err().to_string();
        assert!(err.to_lowercase().contains("read"), "expected a readonly error, got: {err}");
    }

    #[test]
    fn a_message_body_is_capped_before_it_reaches_the_model() {
        let dir = tempfile::tempdir().unwrap();
        let db = fixture_db(dir.path());
        let conn = Connection::open(&db).unwrap();
        conn.execute(
            "INSERT INTO message (ROWID, date, is_from_me, text, cache_has_attachments, service, handle_id)
             VALUES (99, 994224000000000000, 0, ?1, 0, 'iMessage', 1)",
            rusqlite::params!["w".repeat(TEXT_CAP * 3)],
        )
        .unwrap();
        conn.execute("INSERT INTO chat_message_join VALUES (1, 99)", []).unwrap();
        drop(conn);

        let rows = run(&db, "recent", json!({"limit": 1}));
        let text = rows[0]["text"].as_str().unwrap();
        assert!(text.ends_with("… [truncated]"), "a huge message must not flood the turn");
        assert!(text.chars().count() < TEXT_CAP + 20);
    }
}

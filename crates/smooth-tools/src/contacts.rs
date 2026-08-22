//! `contacts` — read the user's macOS Contacts (Address Book), read-only (pearl th-ffa500).
//!
//! ## Why this exists
//!
//! Big Smooth could see that a Scrodes message came from `+18128876048`, but had
//! no way to know that is **Josh** — and no way to turn "text Josh" into a real
//! handle. The [`crate::imessage`] tool hands the model phone numbers and emails;
//! this turns them into names, and names back into handles.
//!
//! ## Why it's a first-class tool and not `bash sqlite3`
//!
//! Same trusted-integration exception the `calendar` and `imessage` tools follow
//! (see `docs/Architecture/Security-Model.md`): the Address Book database lives
//! under `~/Library`, which [`crate::sandbox`]'s profile denies. The read here is
//! **in-process** `rusqlite` on a **read-only** connection — no subprocess, no
//! shell, no injection surface. The TCC grant that matters is Full Disk Access on
//! the daemon's own bundle, exactly as for `imessage`.
//!
//! ## Privacy posture
//!
//! This hands the model the user's private contacts. Safeguards:
//! - Every command **requires a filter** (`name` or `handle`); there is no
//!   "dump the whole address book" shape.
//! - Results are **limited** ([`DEFAULT_LIMIT`], hard cap [`MAX_LIMIT`]) and the
//!   whole reply is capped at [`OUTPUT_CAP`].
//! - **read-only SQL** — the connection carries `SQLITE_OPEN_READ_ONLY` and every
//!   statement is a fixed `SELECT` with bound parameters.
//! - **still Narc-visible** — a normal tool call, so the permission gate and the
//!   Narc hook see it like any other.
//!
//! ## Availability
//! macOS-only (cfg-gated at registration). Registers even when Full Disk Access
//! isn't granted: it answers with actionable setup guidance instead of an opaque
//! failure, because that's something the agent can relay.

#![cfg(target_os = "macos")]

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use rusqlite::types::Value as SqlValue;
use rusqlite::{Connection, OpenFlags};
use serde_json::{json, Value};
use smooth_menubar::setup::{initiate, Grant};
use smooth_operator::{Tool, ToolSchema};

/// Max bytes of output returned before truncation.
const OUTPUT_CAP: usize = 20_000;

/// Rows returned when the caller doesn't say.
const DEFAULT_LIMIT: i64 = 20;

/// Hard ceiling on rows. The privacy floor: no call dumps the whole book.
const MAX_LIMIT: i64 = 100;

/// The commands this tool exposes. An allowlist, not a denylist.
const COMMANDS: &[&str] = &["lookup", "resolve"];

/// The setup instruction handed back whenever the integration isn't usable.
const SETUP_HINT: &str =
    "Contacts isn't readable yet — this needs macOS Full Disk Access (the same grant Messages uses). Run `th doctor --setup-imessage` on the Mac, then try again.";

/// `contacts` — read the user's macOS Contacts, read-only.
pub struct ContactsTool;

/// The Address Book databases to read.
///
/// macOS shards contacts across per-source databases (iCloud, "On My Mac",
/// Exchange, …) under `Sources/<uuid>/`, plus a legacy top-level file. We read
/// whichever exist. `SMOOTH_CONTACTS_DB` overrides with a single explicit path
/// (tests + odd setups).
#[must_use]
pub fn address_book_paths() -> Vec<PathBuf> {
    if let Some(p) = std::env::var_os("SMOOTH_CONTACTS_DB").map(PathBuf::from) {
        return vec![p];
    }
    let Some(home) = dirs_next::home_dir() else {
        return Vec::new();
    };
    let base = home.join("Library").join("Application Support").join("AddressBook");
    let mut out = Vec::new();
    let top = base.join("AddressBook-v22.abcddb");
    if top.exists() {
        out.push(top);
    }
    if let Ok(entries) = std::fs::read_dir(base.join("Sources")) {
        for entry in entries.flatten() {
            let db = entry.path().join("AddressBook-v22.abcddb");
            if db.exists() {
                out.push(db);
            }
        }
    }
    out
}

#[async_trait]
impl Tool for ContactsTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "contacts".into(),
            description: format!(
                "Read the user's REAL macOS Contacts. Turn a name into phone numbers/emails, or a phone number/email into a name. Use it to answer \"what's Josh's number\", to find the handle to text someone with, and to put a name on a number the `imessage` tool returned. Commands: {}. Lookup: {{\"command\":\"lookup\",\"name\":\"Josh\"}} → matching people with their phones + emails (feed a phone/email straight into `imessage` send). Resolve: {{\"command\":\"resolve\",\"handle\":\"+18128876048\"}} → who that is. These are the user's PRIVATE contacts: read only what the question needs, and never repeat them into anything that leaves this conversation. Output is JSON.",
                COMMANDS.join(", ")
            ),
            parameters: json!({
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "enum": COMMANDS,
                        "description": "lookup (name → phones/emails) or resolve (phone/email → name)."
                    },
                    "name": {
                        "type": "string",
                        "description": "Who to look up, for `lookup`. Matched loosely against first/last/full name, nickname, and organization."
                    },
                    "handle": {
                        "type": "string",
                        "description": "A phone number or email to put a name to, for `resolve`. Phone matching ignores formatting (spaces, dashes, +1)."
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
        true
    }

    async fn execute(&self, arguments: Value) -> anyhow::Result<String> {
        let command = command_of(&arguments)?;
        let paths = address_book_paths();
        if paths.is_empty() {
            return Ok(format!("No Contacts database found — the user may have no contacts on this Mac. {SETUP_HINT}"));
        }
        // FDA probe: metadata succeeds on a TCC-denied file, so a byte read is the
        // only honest check. Mirrors `imessage::probe`.
        if let Some(denied) = paths.iter().find(|p| is_permission_denied(p)) {
            return Ok(format!(
                "Big Smooth can't read Contacts at {} — macOS Full Disk Access has not been granted. {}",
                denied.display(),
                initiate(Grant::FullDiskAccess).unwrap_or(SETUP_HINT)
            ));
        }
        let query = build_query(command, &arguments)?;
        let rows = tokio::task::spawn_blocking(move || run_across(&paths, &query)).await??;
        Ok(truncate(&serde_json::to_string_pretty(&rows)?))
    }
}

/// Is this path present-but-unreadable (Full Disk Access denied)?
fn is_permission_denied(path: &Path) -> bool {
    matches!(std::fs::File::open(path), Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied)
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
        .ok_or_else(|| anyhow::anyhow!("`{raw}` is not an allowed contacts command. Allowed: {}", COMMANDS.join(", ")))
}

/// A read to run: fixed SQL plus bound parameters. Built separately from
/// execution so every query shape is unit-testable without a database.
// PartialEq only: `SqlValue` carries an f64 `Real` variant, so it isn't `Eq`
// (same as `imessage::ReadQuery`).
#[derive(Debug, PartialEq)]
pub struct ReadQuery {
    sql: String,
    params: Vec<SqlValue>,
    limit: usize,
}

/// Build the SQL for a command. Every caller value is a bound parameter — no
/// string is ever concatenated into the statement.
///
/// # Errors
/// When the command's required filter (`name` / `handle`) is missing or blank.
pub fn build_query(command: &str, arguments: &Value) -> anyhow::Result<ReadQuery> {
    let limit = limit_of(arguments);
    match command {
        // Name → the person plus every phone/email. We over-fetch joined rows
        // (one per phone/email) and fold them per person in `run_across`, so the
        // SQL LIMIT can't apply here; the fold caps people to `limit`.
        "lookup" => {
            let name = required_str(arguments, "name", "`lookup` needs `name` — who to look up")?;
            Ok(ReadQuery {
                sql: "SELECT r.Z_PK, r.ZFIRSTNAME, r.ZLASTNAME, r.ZORGANIZATION, r.ZNICKNAME, p.ZFULLNUMBER, e.ZADDRESS
                      FROM ZABCDRECORD r
                      LEFT JOIN ZABCDPHONENUMBER p ON p.ZOWNER = r.Z_PK
                      LEFT JOIN ZABCDEMAILADDRESS e ON e.ZOWNER = r.Z_PK
                      WHERE r.ZFIRSTNAME LIKE ?1 OR r.ZLASTNAME LIKE ?1 OR r.ZNICKNAME LIKE ?1
                         OR r.ZORGANIZATION LIKE ?1
                         OR (COALESCE(r.ZFIRSTNAME,'') || ' ' || COALESCE(r.ZLASTNAME,'')) LIKE ?1"
                    .to_owned(),
                params: vec![SqlValue::Text(contains(&name))],
                limit,
            })
        }
        // Handle → name. Phone numbers are matched on their trailing digits so
        // "+1 (812) 887-6048" and "8128876048" cross-match; emails match loosely.
        "resolve" => {
            let handle = required_str(arguments, "handle", "`resolve` needs `handle` — the phone number or email to name")?;
            let digits = digits_only(&handle);
            let (clause, param) = if digits.len() >= 7 {
                // Compare on the last 10 digits (US-centric — a deliberate
                // heuristic; a bound param, so precision-only, never injection).
                let tail = digits.chars().rev().take(10).collect::<String>().chars().rev().collect::<String>();
                (
                    "REPLACE(REPLACE(REPLACE(REPLACE(REPLACE(p.ZFULLNUMBER,' ',''),'-',''),'(',''),')',''),'+','') LIKE ?1",
                    SqlValue::Text(format!("%{tail}")),
                )
            } else {
                ("e.ZADDRESS LIKE ?1", SqlValue::Text(contains(&handle)))
            };
            Ok(ReadQuery {
                sql: format!(
                    "SELECT r.Z_PK, r.ZFIRSTNAME, r.ZLASTNAME, r.ZORGANIZATION, r.ZNICKNAME, p.ZFULLNUMBER, e.ZADDRESS
                     FROM ZABCDRECORD r
                     LEFT JOIN ZABCDPHONENUMBER p ON p.ZOWNER = r.Z_PK
                     LEFT JOIN ZABCDEMAILADDRESS e ON e.ZOWNER = r.Z_PK
                     WHERE {clause}"
                ),
                params: vec![param],
                limit,
            })
        }
        other => anyhow::bail!("`{other}` is not a contacts command"),
    }
}

/// Clamp the caller's `limit` into `1..=MAX_LIMIT`; nonsense falls back to default.
fn limit_of(arguments: &Value) -> usize {
    let n = arguments
        .get("limit")
        .and_then(Value::as_i64)
        .filter(|n| *n > 0)
        .unwrap_or(DEFAULT_LIMIT)
        .min(MAX_LIMIT);
    // n is now in 1..=MAX_LIMIT, so try_from can't fail; the fallback is defensive.
    usize::try_from(n).unwrap_or(20)
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

/// A `LIKE` "contains" pattern.
fn contains(needle: &str) -> String {
    format!("%{needle}%")
}

/// Strip a phone string to digits only.
fn digits_only(s: &str) -> String {
    s.chars().filter(char::is_ascii_digit).collect()
}

/// Run `query` across every Address Book source and fold joined rows into one
/// entry per person (a person with three numbers is one result, not three).
fn run_across(paths: &[PathBuf], query: &ReadQuery) -> anyhow::Result<Vec<Value>> {
    // Keyed by (source-path, Z_PK) so the same person in two sources isn't merged
    // into a mess; insertion order preserved for stable output.
    let mut order: Vec<(String, i64)> = Vec::new();
    let mut people: std::collections::HashMap<(String, i64), Person> = std::collections::HashMap::new();

    for path in paths {
        // a source that won't open shouldn't sink the others
        let Ok(conn) = open_read_only(path) else { continue };
        let mut stmt = conn.prepare(&query.sql)?;
        let params = rusqlite::params_from_iter(query.params.iter());
        let mut rows = stmt.query(params)?;
        let src = path.to_string_lossy().into_owned();
        while let Some(row) = rows.next()? {
            let pk: i64 = row.get(0).unwrap_or(0);
            let key = (src.clone(), pk);
            let entry = people.entry(key.clone()).or_insert_with(|| {
                order.push(key.clone());
                Person::from_row(row)
            });
            entry.absorb(row);
        }
    }

    Ok(order
        .into_iter()
        .take(query.limit)
        .filter_map(|k| people.remove(&k))
        .map(Person::into_json)
        .collect())
}

/// One folded contact.
struct Person {
    name: Option<String>,
    org: Option<String>,
    phones: Vec<String>,
    emails: Vec<String>,
}

impl Person {
    fn from_row(row: &rusqlite::Row<'_>) -> Self {
        let first: Option<String> = row.get(1).unwrap_or(None);
        let last: Option<String> = row.get(2).unwrap_or(None);
        let org: Option<String> = row.get(3).unwrap_or(None);
        let nick: Option<String> = row.get(4).unwrap_or(None);
        let name = full_name(first.as_deref(), last.as_deref(), nick.as_deref());
        Self {
            name,
            org,
            phones: Vec::new(),
            emails: Vec::new(),
        }
    }

    /// Add this joined row's phone/email (the identity columns repeat per row).
    fn absorb(&mut self, row: &rusqlite::Row<'_>) {
        if let Ok(Some(phone)) = row.get::<_, Option<String>>(5) {
            let phone = phone.trim().to_owned();
            if !phone.is_empty() && !self.phones.contains(&phone) {
                self.phones.push(phone);
            }
        }
        if let Ok(Some(email)) = row.get::<_, Option<String>>(6) {
            let email = email.trim().to_owned();
            if !email.is_empty() && !self.emails.contains(&email) {
                self.emails.push(email);
            }
        }
    }

    fn into_json(self) -> Value {
        json!({
            "name": self.name,
            "organization": self.org.filter(|o| !o.is_empty()),
            "phones": self.phones,
            "emails": self.emails,
        })
    }
}

/// Assemble a display name from the parts, falling back gracefully.
fn full_name(first: Option<&str>, last: Option<&str>, nick: Option<&str>) -> Option<String> {
    let joined = [first, last]
        .into_iter()
        .flatten()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    if !joined.is_empty() {
        return Some(joined);
    }
    nick.map(str::trim).filter(|s| !s.is_empty()).map(ToOwned::to_owned)
}

/// A read-only connection, with the `immutable=1` fallback for a WAL db held open
/// by Contacts.app — same trade as [`crate::imessage`]: stale-by-seconds beats
/// failing outright.
fn open_read_only(path: &Path) -> anyhow::Result<Connection> {
    match Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY) {
        Ok(conn) => Ok(conn),
        Err(first) => {
            let uri = format!("file:{}?immutable=1", path.display());
            Connection::open_with_flags(uri, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI)
                .map_err(|second| anyhow::anyhow!("cannot open Contacts at {} ({first}; immutable retry: {second}). {SETUP_HINT}", path.display()))
        }
    }
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
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::cast_possible_truncation,
    clippy::needless_pass_by_value,
    reason = "unwrap/expect/by-value args and small casts are the idiom for test assertions"
)]
mod tests {
    use super::*;

    // ---- schema / argument validation -------------------------------------

    #[test]
    fn schema_names_contacts_and_requires_command() {
        let s = ContactsTool.schema();
        assert_eq!(s.name, "contacts");
        assert_eq!(s.parameters["required"][0], "command");
        assert!(s.description.contains("PRIVATE"), "must warn the model these are private: {}", s.description);
    }

    #[test]
    fn command_allowlist_refuses_anything_else() {
        for bad in ["delete", "add", "export", "dump", ""] {
            assert!(command_of(&json!({"command": bad})).is_err(), "{bad} must be refused");
        }
        for good in COMMANDS {
            assert_eq!(command_of(&json!({"command": good})).unwrap(), *good);
        }
    }

    #[test]
    fn lookup_and_resolve_require_their_filter() {
        // The privacy floor: neither shape is reachable unfiltered.
        assert!(build_query("lookup", &json!({})).unwrap_err().to_string().contains("needs `name`"));
        assert!(build_query("resolve", &json!({"handle": "  "}))
            .unwrap_err()
            .to_string()
            .contains("needs `handle`"));
    }

    #[test]
    fn limit_defaults_and_is_capped() {
        assert_eq!(limit_of(&json!({})), DEFAULT_LIMIT as usize);
        assert_eq!(limit_of(&json!({"limit": 5})), 5);
        assert_eq!(limit_of(&json!({"limit": 9999})), MAX_LIMIT as usize);
        assert_eq!(limit_of(&json!({"limit": 0})), DEFAULT_LIMIT as usize);
        assert_eq!(limit_of(&json!({"limit": -3})), DEFAULT_LIMIT as usize);
    }

    #[test]
    fn queries_are_selects_with_bound_parameters_only() {
        for (cmd, args) in [
            ("lookup", json!({"name": "'; DROP TABLE ZABCDRECORD; --"})),
            ("resolve", json!({"handle": "%' OR 1=1 --"})),
            ("resolve", json!({"handle": "+1 (812) 887-6048"})),
        ] {
            let q = build_query(cmd, &args).unwrap();
            assert!(q.sql.trim_start().starts_with("SELECT"), "{cmd}: {}", q.sql);
            assert!(!q.sql.contains("DROP"), "{cmd}: {}", q.sql);
            assert!(!q.sql.contains("OR 1=1"), "{cmd}: {}", q.sql);
        }
    }

    #[test]
    fn resolve_matches_a_phone_on_trailing_digits() {
        let q = build_query("resolve", &json!({"handle": "+1 (812) 887-6048"})).unwrap();
        assert_eq!(
            q.params[0],
            SqlValue::Text("%8128876048".into()),
            "must match the last 10 digits, formatting-agnostic"
        );
        assert!(q.sql.contains("ZFULLNUMBER"), "{}", q.sql);
    }

    #[test]
    fn resolve_treats_a_short_token_as_an_email() {
        let q = build_query("resolve", &json!({"handle": "josh@example.com"})).unwrap();
        assert_eq!(q.params[0], SqlValue::Text("%josh@example.com%".into()));
        assert!(q.sql.contains("ZADDRESS"), "{}", q.sql);
    }

    #[test]
    fn digits_only_strips_formatting() {
        assert_eq!(digits_only("+1 (812) 887-6048"), "18128876048");
        assert_eq!(digits_only("no digits"), "");
    }

    #[test]
    fn full_name_prefers_first_last_then_nickname() {
        assert_eq!(full_name(Some("Josh"), Some("Heltsley"), None).as_deref(), Some("Josh Heltsley"));
        assert_eq!(full_name(Some("Josh"), None, None).as_deref(), Some("Josh"));
        assert_eq!(full_name(None, None, Some("Big J")).as_deref(), Some("Big J"));
        assert_eq!(full_name(Some("  "), Some(""), None), None);
    }

    // ---- end-to-end against a synthetic AddressBook ------------------------

    fn fixture_db(dir: &Path) -> PathBuf {
        let path = dir.join("AddressBook-v22.abcddb");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE ZABCDRECORD (Z_PK INTEGER PRIMARY KEY, ZFIRSTNAME TEXT, ZLASTNAME TEXT, ZORGANIZATION TEXT, ZNICKNAME TEXT);
             CREATE TABLE ZABCDPHONENUMBER (Z_PK INTEGER PRIMARY KEY, ZOWNER INTEGER, ZFULLNUMBER TEXT);
             CREATE TABLE ZABCDEMAILADDRESS (Z_PK INTEGER PRIMARY KEY, ZOWNER INTEGER, ZADDRESS TEXT);

             INSERT INTO ZABCDRECORD VALUES (1, 'Josh', 'Heltsley', NULL, NULL);
             INSERT INTO ZABCDRECORD VALUES (2, 'Suraj', 'Datta', 'Smoo AI', NULL);
             INSERT INTO ZABCDPHONENUMBER VALUES (10, 1, '(812) 887-6048');
             INSERT INTO ZABCDPHONENUMBER VALUES (11, 1, '+1 812-555-0000');
             INSERT INTO ZABCDEMAILADDRESS VALUES (20, 1, 'josh@example.com');
             INSERT INTO ZABCDPHONENUMBER VALUES (12, 2, '+1 (317) 459-8424');",
        )
        .unwrap();
        path
    }

    /// Build a fresh fixture in its own tempdir per call, so repeated `run`s in
    /// one test never collide on the same on-disk database.
    fn run(_dir: &Path, cmd: &str, args: Value) -> Vec<Value> {
        let dir = tempfile::tempdir().unwrap();
        let db = fixture_db(dir.path());
        run_across(&[db], &build_query(cmd, &args).unwrap()).unwrap()
    }

    #[test]
    fn lookup_folds_multiple_numbers_into_one_person() {
        let dir = tempfile::tempdir().unwrap();
        let rows = run(dir.path(), "lookup", json!({"name": "Josh"}));
        assert_eq!(rows.len(), 1, "one person, not one row per phone: {rows:?}");
        assert_eq!(rows[0]["name"], "Josh Heltsley");
        let phones: Vec<&str> = rows[0]["phones"].as_array().unwrap().iter().map(|p| p.as_str().unwrap()).collect();
        assert_eq!(phones, vec!["(812) 887-6048", "+1 812-555-0000"]);
        assert_eq!(rows[0]["emails"][0], "josh@example.com");
    }

    #[test]
    fn lookup_matches_full_name_and_organization() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(run(dir.path(), "lookup", json!({"name": "Josh Heltsley"})).len(), 1, "full-name match");
        let by_org = run(dir.path(), "lookup", json!({"name": "Smoo"}));
        assert_eq!(by_org.len(), 1);
        assert_eq!(by_org[0]["name"], "Suraj Datta");
    }

    #[test]
    fn resolve_names_a_number_regardless_of_formatting() {
        let dir = tempfile::tempdir().unwrap();
        // The exact scenario from the Scrodes failure: a bare number → a name.
        for handle in ["+18128876048", "8128876048", "(812) 887-6048"] {
            let rows = run(dir.path(), "resolve", json!({"handle": handle}));
            assert_eq!(rows.len(), 1, "{handle} should resolve");
            assert_eq!(rows[0]["name"], "Josh Heltsley", "{handle}");
        }
    }

    #[test]
    fn resolve_names_an_email() {
        let dir = tempfile::tempdir().unwrap();
        let rows = run(dir.path(), "resolve", json!({"handle": "josh@example.com"}));
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["name"], "Josh Heltsley");
    }

    #[test]
    fn an_unknown_handle_resolves_to_nobody() {
        let dir = tempfile::tempdir().unwrap();
        assert!(run(dir.path(), "resolve", json!({"handle": "+19998887777"})).is_empty());
    }

    #[test]
    fn a_sql_injection_attempt_matches_nothing_and_breaks_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let db = fixture_db(dir.path());
        assert!(run_across(
            std::slice::from_ref(&db),
            &build_query("lookup", &json!({"name": "'; DROP TABLE ZABCDRECORD; --"})).unwrap()
        )
        .unwrap()
        .is_empty());
        // Table still there.
        assert_eq!(run_across(&[db], &build_query("lookup", &json!({"name": "Josh"})).unwrap()).unwrap().len(), 1);
    }

    #[test]
    fn the_connection_is_read_only() {
        let dir = tempfile::tempdir().unwrap();
        let db = fixture_db(dir.path());
        let conn = open_read_only(&db).unwrap();
        let err = conn.execute("DELETE FROM ZABCDRECORD", []).unwrap_err().to_string();
        assert!(err.to_lowercase().contains("read"), "expected a readonly error, got: {err}");
    }

    #[test]
    fn limit_caps_the_number_of_people() {
        let dir = tempfile::tempdir().unwrap();
        let db = fixture_db(dir.path());
        // "a" matches Suraj Datta + Josh Heltsley (both contain 'a'/'h'); cap to 1.
        let q = ReadQuery {
            limit: 1,
            ..build_query("lookup", &json!({"name": "a"})).unwrap()
        };
        assert_eq!(run_across(&[db], &q).unwrap().len(), 1);
    }

    #[tokio::test]
    async fn execute_reads_end_to_end_through_the_resolved_path() {
        let dir = tempfile::tempdir().unwrap();
        let db = fixture_db(dir.path());
        std::env::set_var("SMOOTH_CONTACTS_DB", &db);
        assert_eq!(address_book_paths(), vec![db.clone()]);

        let out = ContactsTool.execute(json!({"command": "resolve", "handle": "+18128876048"})).await.unwrap();
        let rows: Vec<Value> = serde_json::from_str(&out).unwrap();
        assert_eq!(rows[0]["name"], "Josh Heltsley");

        std::env::remove_var("SMOOTH_CONTACTS_DB");
    }

    #[test]
    fn setup_hint_points_at_the_grant_command() {
        assert!(SETUP_HINT.contains("th doctor --setup-imessage"));
    }
}

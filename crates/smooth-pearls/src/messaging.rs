//! Mailbox — the Dolt-backed message bus agents poll.
//!
//! Pearl th-70aaef. A message is addressed to a specific agent name or
//! to the literal `all` (broadcast). `read_at IS NULL` means unread.
//! Threads are flat: a reply carries `thread_id = <root message id>`;
//! the root itself has `thread_id = NULL`.
//!
//! Read tracking is per-message (one `read_at`), which is exact for
//! direct messages. For broadcasts (`to = all`) the first reader marks
//! it read for everyone — a deliberate MVP simplification; a
//! per-(message, recipient) read table can come later if broadcast
//! read-state needs to be per-recipient.
//!
//! The `messages` table is created by `PearlStore::open`/`init` and
//! syncs via `refs/dolt/data`, so a message sent in one session is
//! visible to an agent in another after a push/pull (instant for two
//! sessions sharing the same local `.smooth/dolt`).

use anyhow::{Context, Result};
use chrono::{DateTime, NaiveDateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::dolt::SmoothDolt;

/// Recipient name used for broadcast messages.
pub const BROADCAST: &str = "all";

/// A single message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    pub id: String,
    pub from_agent: String,
    /// Recipient agent name, or [`BROADCAST`] (`all`).
    pub to_agent: String,
    pub body: String,
    /// Root message id when this is a reply; `None` for a thread root.
    pub thread_id: Option<String>,
    pub created_at: DateTime<Utc>,
    /// `None` while unread; set to the read time once acknowledged.
    pub read_at: Option<DateTime<Utc>>,
}

impl Message {
    /// The id that identifies this message's thread (its own id when it
    /// is the root, else the root it replied under).
    #[must_use]
    pub fn thread_root(&self) -> &str {
        self.thread_id.as_deref().unwrap_or(&self.id)
    }
}

fn generate_id() -> String {
    let uuid = Uuid::new_v4();
    let hex = uuid.simple().to_string();
    format!("msg-{}", &hex[..8])
}

/// Parse a Dolt datetime string into UTC. smooth-dolt is inconsistent
/// about format: `CURRENT_TIMESTAMP` column defaults come back space-
/// separated (`2026-06-22 16:24:02`) while `NOW()` (used by `mark_read`
/// / `touch`) comes back RFC3339 (`2026-06-22T16:24:02Z`). Accept both,
/// with or without fractional seconds.
pub(crate) fn parse_dolt_datetime(s: &str) -> Option<DateTime<Utc>> {
    if s.is_empty() {
        return None;
    }
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Some(dt.with_timezone(&Utc));
    }
    const FORMATS: &[&str] = &["%Y-%m-%d %H:%M:%S%.f", "%Y-%m-%d %H:%M:%S", "%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%dT%H:%M:%S"];
    FORMATS.iter().find_map(|f| NaiveDateTime::parse_from_str(s, f).ok()).map(|n| n.and_utc())
}

fn parse_dt(value: &Value) -> DateTime<Utc> {
    parse_dolt_datetime(value.as_str().unwrap_or_default()).unwrap_or_else(Utc::now)
}

fn parse_opt_dt(value: &Value) -> Option<DateTime<Utc>> {
    parse_dolt_datetime(value.as_str()?)
}

fn parse_msg(row: &Value) -> Message {
    let thread_id = row["thread_id"].as_str().filter(|s| !s.is_empty()).map(String::from);
    Message {
        id: row["id"].as_str().unwrap_or_default().to_string(),
        from_agent: row["from_agent"].as_str().unwrap_or_default().to_string(),
        to_agent: row["to_agent"].as_str().unwrap_or_default().to_string(),
        body: row["body"].as_str().unwrap_or_default().to_string(),
        thread_id,
        created_at: parse_dt(&row["created_at"]),
        read_at: parse_opt_dt(&row["read_at"]),
    }
}

fn sql_escape(s: &str) -> String {
    s.replace('\'', "''")
}

const COLS: &str = "id, from_agent, to_agent, body, thread_id, created_at, read_at";

/// API over the `messages` table. Cheap to clone.
#[derive(Clone)]
pub struct Mailbox {
    dolt: SmoothDolt,
}

impl Mailbox {
    /// Build a mailbox from an existing handle.
    #[must_use]
    pub fn new(dolt: SmoothDolt) -> Self {
        Self { dolt }
    }

    /// Send a message. `to` is a recipient agent name or [`BROADCAST`].
    /// `thread_id` is the root message id when this is a reply, else
    /// `None`. Returns the new message id.
    ///
    /// # Errors
    /// Returns an error if `from`/`to`/`body` are empty or the write fails.
    pub fn send(&self, from: &str, to: &str, body: &str, thread_id: Option<&str>) -> Result<String> {
        let (from, to, body) = (from.trim(), to.trim(), body.trim());
        if from.is_empty() || to.is_empty() {
            anyhow::bail!("message sender and recipient must not be empty");
        }
        if body.is_empty() {
            anyhow::bail!("message body must not be empty");
        }
        let id = generate_id();
        let thread_sql = thread_id.map_or_else(|| "NULL".to_string(), |t| format!("'{}'", sql_escape(t)));
        let sql = format!(
            "INSERT INTO messages (id, from_agent, to_agent, body, thread_id) VALUES ('{}', '{}', '{}', '{}', {})",
            sql_escape(&id),
            sql_escape(from),
            sql_escape(to),
            sql_escape(body),
            thread_sql,
        );
        self.dolt.exec(&sql).context("insert message")?;
        Ok(id)
    }

    /// Messages addressed to `recipient` (plus broadcasts), oldest first.
    /// When `unread_only`, restricts to `read_at IS NULL`.
    ///
    /// # Errors
    /// Returns an error if the query fails.
    pub fn inbox(&self, recipient: &str, unread_only: bool, limit: usize) -> Result<Vec<Message>> {
        let r = sql_escape(recipient);
        let unread = if unread_only { " AND read_at IS NULL" } else { "" };
        let sql = format!(
            "SELECT {COLS} FROM messages WHERE (to_agent = '{r}' OR to_agent = '{BROADCAST}'){unread} \
             ORDER BY seq ASC LIMIT {limit}"
        );
        let rows = self.dolt.sql(&sql).context("inbox query")?;
        Ok(rows.iter().map(parse_msg).collect())
    }

    /// Messages sent by `sender`, newest first.
    ///
    /// # Errors
    /// Returns an error if the query fails.
    pub fn sent(&self, sender: &str, limit: usize) -> Result<Vec<Message>> {
        let sql = format!(
            "SELECT {COLS} FROM messages WHERE from_agent = '{}' ORDER BY seq DESC LIMIT {limit}",
            sql_escape(sender)
        );
        let rows = self.dolt.sql(&sql).context("sent query")?;
        Ok(rows.iter().map(parse_msg).collect())
    }

    /// Fetch one message by id.
    ///
    /// # Errors
    /// Returns an error if the query fails.
    pub fn get(&self, id: &str) -> Result<Option<Message>> {
        let sql = format!("SELECT {COLS} FROM messages WHERE id = '{}' LIMIT 1", sql_escape(id));
        let rows = self.dolt.sql(&sql).context("get message")?;
        Ok(rows.first().map(parse_msg))
    }

    /// All messages in a thread (the root plus its replies), oldest
    /// first. `root_id` is the thread root message id.
    ///
    /// # Errors
    /// Returns an error if the query fails.
    pub fn thread(&self, root_id: &str) -> Result<Vec<Message>> {
        let r = sql_escape(root_id);
        let sql = format!("SELECT {COLS} FROM messages WHERE id = '{r}' OR thread_id = '{r}' ORDER BY seq ASC");
        let rows = self.dolt.sql(&sql).context("thread query")?;
        Ok(rows.iter().map(parse_msg).collect())
    }

    /// Mark a message read (idempotent — only sets `read_at` if unset).
    ///
    /// # Errors
    /// Returns an error if the write fails.
    pub fn mark_read(&self, id: &str) -> Result<()> {
        let sql = format!("UPDATE messages SET read_at = NOW() WHERE id = '{}' AND read_at IS NULL", sql_escape(id));
        self.dolt.exec(&sql).context("mark message read")?;
        Ok(())
    }

    /// Mark every unread message addressed to `recipient` (incl.
    /// broadcasts) read. Returns nothing; idempotent.
    ///
    /// # Errors
    /// Returns an error if the write fails.
    pub fn mark_all_read(&self, recipient: &str) -> Result<()> {
        let r = sql_escape(recipient);
        let sql = format!("UPDATE messages SET read_at = NOW() WHERE (to_agent = '{r}' OR to_agent = '{BROADCAST}') AND read_at IS NULL");
        self.dolt.exec(&sql).context("mark all read")?;
        Ok(())
    }

    /// Count unread messages for `recipient` (incl. broadcasts).
    ///
    /// # Errors
    /// Returns an error if the query fails.
    pub fn unread_count(&self, recipient: &str) -> Result<usize> {
        let r = sql_escape(recipient);
        let sql = format!("SELECT COUNT(*) AS n FROM messages WHERE (to_agent = '{r}' OR to_agent = '{BROADCAST}') AND read_at IS NULL");
        let rows = self.dolt.sql(&sql).context("unread count")?;
        let n = rows.first().and_then(|r| r["n"].as_u64()).unwrap_or(0);
        Ok(usize::try_from(n).unwrap_or(0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::PearlStore;
    use tempfile::TempDir;

    fn mailbox() -> (TempDir, Mailbox) {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join(".smooth").join("dolt");
        let store = PearlStore::init(&dir).expect("init");
        (tmp, Mailbox::new(store.dolt().clone()))
    }

    #[test]
    fn send_then_inbox_direct() {
        let (_t, mb) = mailbox();
        mb.send("alice", "bob", "hello bob", None).unwrap();
        let bob = mb.inbox("bob", false, 50).unwrap();
        assert_eq!(bob.len(), 1);
        assert_eq!(bob[0].from_agent, "alice");
        assert_eq!(bob[0].body, "hello bob");
        assert!(bob[0].read_at.is_none());
        // Not addressed to carol.
        assert_eq!(mb.inbox("carol", false, 50).unwrap().len(), 0);
    }

    #[test]
    fn broadcast_reaches_everyone() {
        let (_t, mb) = mailbox();
        mb.send("alice", super::BROADCAST, "all hands", None).unwrap();
        assert_eq!(mb.inbox("bob", false, 50).unwrap().len(), 1);
        assert_eq!(mb.inbox("carol", false, 50).unwrap().len(), 1);
    }

    #[test]
    fn unread_filter_and_mark_read() {
        let (_t, mb) = mailbox();
        let id = mb.send("alice", "bob", "ping", None).unwrap();
        assert_eq!(mb.unread_count("bob").unwrap(), 1);
        assert_eq!(mb.inbox("bob", true, 50).unwrap().len(), 1);
        mb.mark_read(&id).unwrap();
        assert_eq!(mb.unread_count("bob").unwrap(), 0);
        assert_eq!(mb.inbox("bob", true, 50).unwrap().len(), 0);
        // Still visible without the unread filter, now with a read_at.
        let all = mb.inbox("bob", false, 50).unwrap();
        assert_eq!(all.len(), 1);
        assert!(all[0].read_at.is_some());
    }

    #[test]
    fn threads_group_root_and_replies() {
        let (_t, mb) = mailbox();
        let root = mb.send("alice", "bob", "question?", None).unwrap();
        mb.send("bob", "alice", "answer", Some(&root)).unwrap();
        mb.send("alice", "bob", "thanks", Some(&root)).unwrap();
        let thread = mb.thread(&root).unwrap();
        assert_eq!(thread.len(), 3, "root + 2 replies");
        assert_eq!(thread[0].id, root);
        assert_eq!(thread[1].thread_root(), root);
    }

    #[test]
    fn mark_all_read_clears_inbox() {
        let (_t, mb) = mailbox();
        mb.send("a", "bob", "1", None).unwrap();
        mb.send("a", "bob", "2", None).unwrap();
        mb.send("a", super::BROADCAST, "3", None).unwrap();
        assert_eq!(mb.unread_count("bob").unwrap(), 3);
        mb.mark_all_read("bob").unwrap();
        assert_eq!(mb.unread_count("bob").unwrap(), 0);
    }

    #[test]
    fn empty_fields_rejected() {
        let (_t, mb) = mailbox();
        assert!(mb.send("", "bob", "x", None).is_err());
        assert!(mb.send("a", "", "x", None).is_err());
        assert!(mb.send("a", "bob", "   ", None).is_err());
    }

    #[test]
    fn quotes_in_body_are_escaped() {
        let (_t, mb) = mailbox();
        mb.send("a", "bob", "it's a 'quoted' body", None).unwrap();
        assert_eq!(mb.inbox("bob", false, 50).unwrap()[0].body, "it's a 'quoted' body");
    }
}

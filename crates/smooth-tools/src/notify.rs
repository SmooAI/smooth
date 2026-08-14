//! `notify` — Big Smooth proactively pushes a notification to the user's devices
//! (installed PWA / phone) via the daemon's web-push + platform self-notify
//! infra (pearl th-c29d34).
//!
//! The tool is thin: it validates + clamps the message, applies the family
//! audience clearance, and hands off to a [`NotifySink`]. The sink is
//! implemented by the daemon (over `push::PushState` + the platform
//! self-notify route), abstracted here so `smooth-tools` needn't depend on the
//! daemon — same seam as `send_file`'s directive sink.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};
use smooth_operator::{Tool, ToolSchema};

use crate::util::req_str;

/// Lock-screen length caps (chars).
const TITLE_MAX: usize = 120;
const BODY_MAX: usize = 240;

/// Delivers a notification to the user's devices.
///
/// Best-effort; returns the number of device legs reached. Implemented by the
/// daemon; abstracted so the tool crate stays free of push/credential deps.
#[async_trait]
pub trait NotifySink: Send + Sync {
    async fn deliver(&self, title: &str, body: &str, deep_link: Option<&str>) -> usize;
}

/// `notify` — proactively reach the user on their devices.
pub struct NotifyTool {
    sink: Arc<dyn NotifySink>,
    /// Whether the calling principal is the daemon **owner** (no family
    /// `role:` group). A family member/child principal is confined to notifying
    /// *itself* — it may not broadcast to "all" or aim at another member. Bound
    /// from the authenticated role at construction (never from tool args, which
    /// a child controls), mirroring the deny-by-default family tool filter and
    /// the per-role calendar allowlist.
    owner: bool,
}

impl NotifyTool {
    #[must_use]
    pub fn new(sink: Arc<dyn NotifySink>, owner: bool) -> Self {
        Self { sink, owner }
    }
}

/// Trim + clamp to `max` chars, ellipsizing (char-boundary safe).
fn clamp(s: &str, max: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= max {
        return s.to_string();
    }
    let clipped: String = s.chars().take(max - 1).collect();
    format!("{clipped}…")
}

#[async_trait]
impl Tool for NotifyTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "notify".into(),
            description: "Proactively push a notification to the user's devices (their phone or installed app) — for a reminder you set, a long-running job finishing, or a genuinely useful heads-up. Reaches them even when the chat is closed. Use sparingly: notifications interrupt. NOT for chatty replies or restating something you already answered inline.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "title": { "type": "string", "description": "Short notification title (a few words)." },
                    "body": { "type": "string", "description": "One-line body — what happened or what to do next." },
                    "deepLink": { "type": "string", "description": "Optional app deep link to open on tap (e.g. bigsmooth://chat)." },
                    "audience": { "type": "string", "description": "Who to notify: \"all\" (default — every device) or a specific family member/role id. A non-owner principal may only notify itself." }
                },
                "required": ["title", "body"]
            }),
        }
    }

    async fn execute(&self, arguments: Value) -> anyhow::Result<String> {
        let title = clamp(&req_str(&arguments, "title")?, TITLE_MAX);
        let body = clamp(&req_str(&arguments, "body")?, BODY_MAX);
        if title.is_empty() && body.is_empty() {
            return Ok("BLOCKED: a notification needs a title or body.".into());
        }
        let deep_link = arguments.get("deepLink").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty());

        // Audience clearance (ADR-008 family RBAC). Owner: unrestricted. A family
        // member/child may only aim at "all"/"self" (both delivered to its own
        // devices) — naming another member is refused. Whether this principal has
        // the `notify` tool *at all* is already decided upstream by the
        // deny-by-default family filter in the daemon's `tools_for`.
        //
        // ponytail: per-audience ROUTING isn't wired yet — web-push subs aren't
        // principal-tagged, so `deliver` fans out to the owner's devices
        // regardless of who was targeted. The clearance CHECK is real; add
        // routing when subs carry a principal id (the mobile receive side, smooai).
        let requested = arguments
            .get("audience")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("all");
        if !self.owner && requested != "all" && requested != "self" {
            return Ok(format!("BLOCKED: you can only notify yourself, not \"{requested}\"."));
        }

        let reached = self.sink.deliver(&title, &body, deep_link).await;
        Ok(format!("Notification sent ({reached} device leg(s) reached)."))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "unwrap is the idiom for test assertions")]
mod tests {
    use std::sync::Mutex;

    use super::*;

    /// Records what it was asked to deliver; returns a fixed reach count.
    #[derive(Default)]
    struct SpySink {
        calls: Mutex<Vec<(String, String, Option<String>)>>,
    }

    #[async_trait]
    impl NotifySink for SpySink {
        async fn deliver(&self, title: &str, body: &str, deep_link: Option<&str>) -> usize {
            self.calls.lock().unwrap().push((title.into(), body.into(), deep_link.map(str::to_owned)));
            2
        }
    }

    fn tool(owner: bool) -> (NotifyTool, Arc<SpySink>) {
        let sink = Arc::new(SpySink::default());
        (NotifyTool::new(sink.clone(), owner), sink)
    }

    #[tokio::test]
    async fn owner_notifies_and_reports_reach() {
        let (t, sink) = tool(true);
        let out = t
            .execute(json!({ "title": "Done", "body": "Build finished", "deepLink": "bigsmooth://chat" }))
            .await
            .unwrap();
        assert!(out.contains("2 device"), "{out}");
        let calls = sink.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0], ("Done".into(), "Build finished".into(), Some("bigsmooth://chat".into())));
    }

    #[tokio::test]
    async fn owner_may_target_a_specific_member() {
        let (t, sink) = tool(true);
        let out = t.execute(json!({ "title": "hi", "body": "there", "audience": "mom" })).await.unwrap();
        assert!(out.contains("device leg"), "{out}");
        assert_eq!(sink.calls.lock().unwrap().len(), 1, "owner delivery goes through");
    }

    #[tokio::test]
    async fn child_cannot_target_another_member() {
        let (t, sink) = tool(false);
        let out = t.execute(json!({ "title": "hi", "body": "there", "audience": "dad" })).await.unwrap();
        assert!(out.starts_with("BLOCKED"), "{out}");
        assert!(sink.calls.lock().unwrap().is_empty(), "blocked delivery never reaches the sink");
    }

    #[tokio::test]
    async fn child_may_notify_self_or_all() {
        for audience in ["self", "all"] {
            let (t, sink) = tool(false);
            let out = t.execute(json!({ "title": "hi", "body": "there", "audience": audience })).await.unwrap();
            assert!(out.contains("device leg"), "audience {audience}: {out}");
            assert_eq!(sink.calls.lock().unwrap().len(), 1);
        }
    }

    #[tokio::test]
    async fn clamps_overlong_fields() {
        let (t, sink) = tool(true);
        t.execute(json!({ "title": "x".repeat(500), "body": "y".repeat(500) })).await.unwrap();
        let calls = sink.calls.lock().unwrap();
        assert_eq!(calls[0].0.chars().count(), TITLE_MAX);
        assert_eq!(calls[0].1.chars().count(), BODY_MAX);
    }

    #[tokio::test]
    async fn requires_title_and_body() {
        let (t, _) = tool(true);
        assert!(t.execute(json!({ "title": "only title" })).await.is_err(), "missing body errors");
    }
}

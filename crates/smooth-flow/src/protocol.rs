//! The v0 flow wire protocol.
//!
//! Every frame is one JSON object `{"channel":"flow","type":"<name>", ...}`.
//! Unknown types are ignored, never fatal. Also the hook → state mapping
//! table, which is pure so the whole contract is unit-testable without
//! sockets.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::harness::HarnessInfo;
use crate::store::{Attention, FanOut, Session, SessionKind, SessionState};

/// The `channel` value every flow frame carries.
pub const CHANNEL: &str = "flow";

/// One fan-out candidate spec.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CandidateSpec {
    pub kind: SessionKind,
    #[serde(default)]
    pub model: Option<String>,
    pub label: String,
}

/// Client → engine frames.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ClientFrame {
    #[serde(rename = "flow.attach")]
    Attach { id: String, cols: u16, rows: u16 },
    #[serde(rename = "flow.detach")]
    Detach { id: String },
    #[serde(rename = "flow.input")]
    Input { id: String, data_b64: String },
    #[serde(rename = "flow.resize")]
    Resize { id: String, cols: u16, rows: u16 },
    #[serde(rename = "flow.snapshot")]
    Snapshot { id: String },
    #[serde(rename = "flow.new")]
    New {
        kind: SessionKind,
        #[serde(default)]
        worktree: Option<String>,
        #[serde(default)]
        project: Option<String>,
        #[serde(default)]
        pearl_id: Option<String>,
        #[serde(default)]
        prompt: Option<String>,
        #[serde(default)]
        argv: Option<Vec<String>>,
        #[serde(default)]
        title: Option<String>,
        /// Additive (th-d33afa): the tmux socket (`tmux -L <name>`) to create
        /// the session on, instead of the daemon's default. The macOS shell
        /// owns `tmux -L smoothflow` so TCC attribution holds.
        #[serde(default)]
        tmux_socket: Option<String>,
    },
    #[serde(rename = "flow.send")]
    Send { id: String, text: String },
    #[serde(rename = "flow.approve")]
    Approve { id: String, request_id: String, decision: Decision },
    #[serde(rename = "flow.kill")]
    Kill {
        id: String,
        #[serde(default)]
        resume: bool,
    },
    #[serde(rename = "flow.fanout.new")]
    FanoutNew {
        prompt: String,
        pearl_id: String,
        candidates: Vec<CandidateSpec>,
        /// Additive to the v0 spec: the main checkout to fan out from. Defaults
        /// to the daemon's workspace when absent.
        #[serde(default)]
        project: Option<String>,
    },
    #[serde(rename = "flow.fanout.pick")]
    FanoutPick { fan_out_id: String, winner_session_id: String },
    #[serde(rename = "flow.mark_read")]
    MarkRead { id: String },
    /// Additive (th-d33afa): a client asking for `flow.hello` again — the
    /// phone's bridge nudge, since over the relay nothing opens the flow WS
    /// until the phone sends a frame.
    #[serde(rename = "flow.hello")]
    Hello {},
    /// Additive (th-d33afa): the pearl-rail packet over WS (the relay brokers
    /// WS only). Reply is `ServerFrame::Handoff`.
    #[serde(rename = "flow.handoff")]
    Handoff { id: String },
    /// Additive (th-e126cc): finish a session for good — close its pearl,
    /// remove its worktree (and branch) once the branch is merged, drop the
    /// row. Reply is `flow.session.removed` (or `flow.error`, with nothing
    /// touched). `force` removes a dirty or unmerged worktree.
    #[serde(rename = "flow.close")]
    Close {
        id: String,
        #[serde(default)]
        close_pearl: bool,
        #[serde(default)]
        remove_worktree: bool,
        #[serde(default)]
        force: bool,
    },
}

/// What `flow.close` did (the HTTP reply of `POST /api/flow/sessions/{id}/close`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CloseOutcome {
    pub id: String,
    /// The pearl `th pearls close` closed.
    #[serde(default)]
    pub pearl_closed: Option<String>,
    /// The worktree path removed.
    #[serde(default)]
    pub worktree_removed: Option<String>,
    /// The branch deleted with it.
    #[serde(default)]
    pub branch_deleted: Option<String>,
}

/// Who an event line belongs to (the phone's Chat tab).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EventKind {
    User,
    Agent,
    Tool,
    System,
}

impl EventKind {
    /// The wire/storage spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Agent => "agent",
            Self::Tool => "tool",
            Self::System => "system",
        }
    }
}

impl std::str::FromStr for EventKind {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> anyhow::Result<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "user" => Ok(Self::User),
            "agent" => Ok(Self::Agent),
            "tool" => Ok(Self::Tool),
            "system" => Ok(Self::System),
            other => Err(anyhow::anyhow!("unknown event kind `{other}`")),
        }
    }
}

/// One line of a session's event stream (`flow.event`, th-d33afa).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FlowEvent {
    pub event_id: String,
    pub at: chrono::DateTime<chrono::Utc>,
    pub kind: EventKind,
    pub text: String,
}

/// A `flow.approve` decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    Allow,
    Deny,
    AllowSession,
}

impl Decision {
    /// The wire spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::Deny => "deny",
            Self::AllowSession => "allow_session",
        }
    }
}

/// Parse one client frame. `Ok(None)` for a frame of an unknown type (or a
/// non-flow channel) — ignored by contract; `Err` for a known type whose
/// fields don't parse, or non-JSON.
///
/// # Errors
/// When `text` is not JSON, or a known `type` has malformed fields.
pub fn parse_client_frame(text: &str) -> anyhow::Result<Option<ClientFrame>> {
    let v: Value = serde_json::from_str(text)?;
    if let Some(ch) = v.get("channel").and_then(Value::as_str) {
        if ch != CHANNEL {
            return Ok(None);
        }
    }
    let Some(ty) = v.get("type").and_then(Value::as_str) else { return Ok(None) };
    if !ty.starts_with("flow.") {
        return Ok(None);
    }
    match serde_json::from_value::<ClientFrame>(v.clone()) {
        Ok(f) => Ok(Some(f)),
        // Unknown variant ⇒ ignore; anything else is a malformed known frame.
        Err(e) if e.to_string().contains("unknown variant") => Ok(None),
        Err(e) => Err(anyhow::anyhow!("malformed {ty}: {e}")),
    }
}

/// The client-side `seq` a frame may carry, echoed back in `flow.error.ref`.
#[must_use]
pub fn client_seq(text: &str) -> Option<Value> {
    serde_json::from_str::<Value>(text).ok()?.get("seq").cloned()
}

/// Engine → client frames.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
#[allow(clippy::large_enum_variant, reason = "a Session row rides in the broadcast; boxing buys nothing at 4096 slots")]
pub enum ServerFrame {
    #[serde(rename = "flow.hello")]
    Hello {
        daemon: DaemonInfo,
        sessions: Vec<Session>,
        /// Additive (th-0f6126): the harnesses a picker offers, in the
        /// user's order, hidden ones dropped.
        #[serde(default)]
        harnesses: Vec<HarnessInfo>,
    },
    /// Additive (th-0f6126): the visible harness list changed
    /// (`PUT /api/flow/harnesses/prefs`); same shape as `flow.hello.harnesses`.
    #[serde(rename = "flow.harnesses")]
    Harnesses { harnesses: Vec<HarnessInfo> },
    #[serde(rename = "flow.session")]
    Session { session: Session },
    #[serde(rename = "flow.session.removed")]
    SessionRemoved { id: String },
    #[serde(rename = "flow.output")]
    Output { id: String, seq: u64, data_b64: String },
    #[serde(rename = "flow.screen")]
    Screen { id: String, cols: u16, rows: u16, text: String },
    #[serde(rename = "flow.attention")]
    Attention { id: String, attention: Option<Attention> },
    #[serde(rename = "flow.fanout")]
    Fanout { fan_out: FanOut, candidates: Vec<Session> },
    #[serde(rename = "flow.error")]
    Error {
        #[serde(rename = "ref")]
        r#ref: Option<Value>,
        code: String,
        message: String,
    },
    /// Additive (th-d33afa): one event line of a session's stream.
    #[serde(rename = "flow.event")]
    Event {
        id: String,
        #[serde(flatten)]
        event: FlowEvent,
    },
    /// Additive (th-d33afa): the pearl-rail packet, same shape as
    /// `GET /api/flow/sessions/{id}/handoff` plus the session `id`.
    #[serde(rename = "flow.handoff")]
    Handoff {
        id: String,
        pearl: Value,
        handoff: Value,
        checkpoints: Value,
        blocks: Value,
        pr: Value,
    },
}

/// The `daemon` block of `flow.hello`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonInfo {
    pub version: String,
    pub machine_label: String,
}

impl ServerFrame {
    /// The wire JSON, with `"channel":"flow"` added.
    #[must_use]
    pub fn to_wire(&self) -> String {
        let mut v = serde_json::to_value(self).unwrap_or_else(|_| json!({}));
        if let Some(obj) = v.as_object_mut() {
            obj.insert("channel".into(), Value::String(CHANNEL.into()));
        }
        v.to_string()
    }

    /// Build an error frame.
    #[must_use]
    pub fn error(r#ref: Option<Value>, code: &str, message: impl Into<String>) -> Self {
        Self::Error {
            r#ref,
            code: code.to_string(),
            message: message.into(),
        }
    }

    /// The session id an output frame targets, if this is one.
    #[must_use]
    pub fn output_session(&self) -> Option<&str> {
        match self {
            Self::Output { id, .. } => Some(id),
            _ => None,
        }
    }
}

/// Parse a wire string back into a server frame (clients + tests).
///
/// # Errors
/// When the text is not a known server frame.
pub fn parse_server_frame(text: &str) -> anyhow::Result<ServerFrame> {
    Ok(serde_json::from_str(text)?)
}

/// True when a relay/wire frame belongs to the flow channel.
#[must_use]
pub fn is_flow_frame(v: &Value) -> bool {
    v.get("channel").and_then(Value::as_str) == Some(CHANNEL)
}

// ── hooks ─────────────────────────────────────────────────────────────────────

/// `POST /api/flow/hooks` body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HookEvent {
    #[serde(default)]
    pub harness: String,
    pub event: String,
    pub session_id: String,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub payload: Value,
}

/// What a hook event means for the session's state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookOutcome {
    /// Move to `working`.
    Working,
    /// Move to `idle` and mark unread.
    Idle,
    /// Move to `needs_you` with this attention (`permission` or `question`).
    NeedsYou(Attention),
    /// The harness session ended (`SessionEnd`); exit is decided by the PTY.
    Ended,
    /// Nothing to do (SessionStart, PreCompact, SubagentStop, unknown).
    None,
}

/// Summarise a permission payload for the attention detail: `Tool: input`.
#[must_use]
pub fn permission_detail(payload: &Value) -> String {
    let tool = payload.get("tool_name").and_then(Value::as_str).unwrap_or("tool");
    let input = payload.get("tool_input").cloned().unwrap_or(Value::Null);
    let brief = input
        .get("command")
        .or_else(|| input.get("file_path"))
        .or_else(|| input.get("path"))
        .or_else(|| input.get("pattern"))
        .and_then(Value::as_str)
        .map_or_else(|| if input.is_null() { String::new() } else { input.to_string() }, str::to_string);
    let brief: String = brief.chars().take(200).collect();
    if brief.is_empty() {
        tool.to_string()
    } else {
        format!("{tool}: {brief}")
    }
}

/// The state-mapping table from the spec:
/// `UserPromptSubmit|PreToolUse|PostToolUse` ⇒ working; `Stop` ⇒ idle;
/// `PermissionRequest|Notification(permission|question)` ⇒ needs_you;
/// `SessionEnd` ⇒ ended.
#[must_use]
pub fn map_hook_event(event: &str, payload: &Value) -> HookOutcome {
    match event {
        "UserPromptSubmit" | "PreToolUse" | "PostToolUse" => HookOutcome::Working,
        "Stop" => HookOutcome::Idle,
        "PermissionRequest" => HookOutcome::NeedsYou(Attention::new("permission").with_detail(permission_detail(payload))),
        "Notification" => {
            let ntype = payload.get("notification_type").and_then(Value::as_str).unwrap_or("");
            let message = payload.get("message").and_then(Value::as_str).unwrap_or("").to_string();
            let lower = format!("{ntype} {message}").to_lowercase();
            if lower.contains("permission") {
                HookOutcome::NeedsYou(Attention::new("permission").with_detail(message))
            } else if lower.contains("question") || lower.contains("waiting for your input") || lower.contains("idle") {
                HookOutcome::NeedsYou(Attention::new("question").with_detail(message))
            } else {
                HookOutcome::None
            }
        }
        "SessionEnd" => HookOutcome::Ended,
        _ => HookOutcome::None,
    }
}

/// The event line a hook produces for the Chat tab, if any (th-d33afa).
///
/// The user's prompt, each tool call, the agent's final message on `Stop`,
/// and system lines for notifications and session end. A permission
/// request produces no line here — its state change does (`needs_you`
/// with the attention detail), so it isn't reported twice.
#[must_use]
pub fn hook_event_text(event: &str, payload: &Value) -> Option<(EventKind, String)> {
    let text = |k: &str| {
        payload
            .get(k)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(str::to_string)
    };
    match event {
        "UserPromptSubmit" => text("prompt").map(|t| (EventKind::User, t)),
        "PreToolUse" => {
            let tool = payload.get("tool_name").and_then(Value::as_str).unwrap_or("tool");
            let detail = permission_detail(payload);
            let brief = detail.strip_prefix(tool).and_then(|d| d.strip_prefix(": ")).unwrap_or("");
            Some((EventKind::Tool, format!("● {tool}({brief})")))
        }
        "Stop" => text("last_assistant_message").map(|t| (EventKind::Agent, t)),
        "Notification" => text("message").map(|t| (EventKind::System, t)),
        "SessionEnd" => Some((
            EventKind::System,
            format!("session ended{}", text("reason").map(|r| format!(" ({r})")).unwrap_or_default()),
        )),
        _ => None,
    }
}

/// The state a hook outcome lands in, if any.
#[must_use]
pub const fn outcome_state(outcome: &HookOutcome) -> Option<SessionState> {
    match outcome {
        HookOutcome::Working => Some(SessionState::Working),
        HookOutcome::Idle => Some(SessionState::Idle),
        HookOutcome::NeedsYou(_) => Some(SessionState::NeedsYou),
        HookOutcome::Ended | HookOutcome::None => None,
    }
}

/// The JSON Claude Code expects back from a `PermissionRequest` hook.
/// `AllowSession` allows and adds a session-scoped allow rule for the tool.
#[must_use]
pub fn permission_reply(decision: Decision, payload: &Value) -> Value {
    let tool = payload.get("tool_name").and_then(Value::as_str);
    let decision_v = match decision {
        Decision::Allow => json!({ "behavior": "allow" }),
        Decision::Deny => json!({ "behavior": "deny", "message": "Denied from SmoothFlow" }),
        Decision::AllowSession => {
            let mut d = json!({ "behavior": "allow" });
            if let Some(tool) = tool {
                d["updatedPermissions"] = json!([{
                    "type": "addRules",
                    "rules": [{ "toolName": tool }],
                    "behavior": "allow",
                    "destination": "session"
                }]);
            }
            d
        }
    };
    json!({
        "hookSpecificOutput": {
            "hookEventName": "PermissionRequest",
            "decision": decision_v
        }
    })
}

/// The keystroke that answers a *scraped* (non-hook) Claude Code approval
/// menu: `1` = yes, `2` = yes and don't ask again, `Escape` = no.
#[must_use]
pub const fn approval_keystroke(decision: Decision) -> &'static str {
    match decision {
        Decision::Allow => "1",
        Decision::AllowSession => "2",
        Decision::Deny => "Escape",
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "unwrap is the idiom for test assertions")]
mod tests {
    use super::*;
    use chrono::Utc;

    fn session() -> Session {
        Session {
            id: "fs-00000001".into(),
            kind: SessionKind::Claude,
            title: "t".into(),
            project: "/p".into(),
            worktree: "/p".into(),
            branch: None,
            pearl_id: None,
            agent_session_id: Some("u".into()),
            argv: vec!["claude".into()],
            tmux_session: None,
            tmux_socket: Some("smooth-flow".into()),
            state_source: "inferred".into(),
            pid: None,
            pid_start: Some(1),
            state: SessionState::Working,
            attention: None,
            fan_out_id: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            ended_at: None,
            exit_code: None,
            unread: false,
        }
    }

    #[test]
    fn client_frames_round_trip() {
        let frames = vec![
            ClientFrame::Attach {
                id: "a".into(),
                cols: 80,
                rows: 24,
            },
            ClientFrame::Detach { id: "a".into() },
            ClientFrame::Input {
                id: "a".into(),
                data_b64: "aGk=".into(),
            },
            ClientFrame::Resize {
                id: "a".into(),
                cols: 1,
                rows: 2,
            },
            ClientFrame::Snapshot { id: "a".into() },
            ClientFrame::New {
                kind: SessionKind::Claude,
                worktree: None,
                project: Some("/p".into()),
                pearl_id: Some("th-1".into()),
                prompt: Some("hi".into()),
                argv: None,
                title: None,
                tmux_socket: Some("smoothflow".into()),
            },
            ClientFrame::Send {
                id: "a".into(),
                text: "go".into(),
            },
            ClientFrame::Approve {
                id: "a".into(),
                request_id: "r".into(),
                decision: Decision::AllowSession,
            },
            ClientFrame::Kill { id: "a".into(), resume: true },
            ClientFrame::FanoutNew {
                prompt: "p".into(),
                pearl_id: "th-1".into(),
                candidates: vec![CandidateSpec {
                    kind: SessionKind::Claude,
                    model: Some("opus".into()),
                    label: "a".into(),
                }],
                project: None,
            },
            ClientFrame::FanoutPick {
                fan_out_id: "fo-1".into(),
                winner_session_id: "fs-1".into(),
            },
            ClientFrame::MarkRead { id: "a".into() },
        ];
        for f in frames {
            let mut v = serde_json::to_value(&f).unwrap();
            v["channel"] = json!("flow");
            let back = parse_client_frame(&v.to_string()).unwrap().unwrap();
            assert_eq!(back, f);
        }
    }

    #[test]
    fn additive_phone_frames_round_trip() {
        let hello = parse_client_frame(r#"{"channel":"flow","type":"flow.hello"}"#).unwrap().unwrap();
        assert_eq!(hello, ClientFrame::Hello {});
        let h = parse_client_frame(r#"{"channel":"flow","type":"flow.handoff","id":"fs-1"}"#).unwrap().unwrap();
        assert_eq!(h, ClientFrame::Handoff { id: "fs-1".into() });
        // th-e126cc: every flag defaults off, so a bare close only drops the row.
        let c = parse_client_frame(r#"{"channel":"flow","type":"flow.close","id":"fs-1"}"#).unwrap().unwrap();
        assert_eq!(
            c,
            ClientFrame::Close {
                id: "fs-1".into(),
                close_pearl: false,
                remove_worktree: false,
                force: false
            }
        );
        let c = parse_client_frame(r#"{"channel":"flow","type":"flow.close","id":"fs-1","close_pearl":true,"remove_worktree":true,"force":true}"#)
            .unwrap()
            .unwrap();
        assert!(matches!(
            c,
            ClientFrame::Close {
                close_pearl: true,
                remove_worktree: true,
                force: true,
                ..
            }
        ));
        let out: CloseOutcome = serde_json::from_str(r#"{"id":"fs-1","pearl_closed":"th-1"}"#).unwrap();
        assert_eq!(out.pearl_closed.as_deref(), Some("th-1"));
        assert!(out.worktree_removed.is_none() && out.branch_deleted.is_none());

        let ev = ServerFrame::Event {
            id: "fs-1".into(),
            event: FlowEvent {
                event_id: "fs-1-0".into(),
                at: Utc::now(),
                kind: EventKind::Tool,
                text: "● Bash(ls)".into(),
            },
        };
        let v: Value = serde_json::from_str(&ev.to_wire()).unwrap();
        assert_eq!(v["type"], "flow.event");
        assert_eq!(v["kind"], "tool", "event fields are flattened to the top level: {v}");
        assert_eq!(v["event_id"], "fs-1-0");
        assert!(v["at"].is_string());
        assert_eq!(parse_server_frame(&ev.to_wire()).unwrap().to_wire(), ev.to_wire());

        let ho = ServerFrame::Handoff {
            id: "fs-1".into(),
            pearl: Value::Null,
            handoff: json!({"branch":"b"}),
            checkpoints: json!([]),
            blocks: json!([]),
            pr: Value::Null,
        };
        let v: Value = serde_json::from_str(&ho.to_wire()).unwrap();
        assert_eq!(v["type"], "flow.handoff");
        assert_eq!(v["handoff"]["branch"], "b");
        assert_eq!(parse_server_frame(&ho.to_wire()).unwrap(), ho);
    }

    #[test]
    fn hook_event_text_table() {
        assert_eq!(
            hook_event_text("UserPromptSubmit", &json!({"prompt":" fix it "})),
            Some((EventKind::User, "fix it".into()))
        );
        assert_eq!(hook_event_text("UserPromptSubmit", &json!({})), None);
        assert_eq!(
            hook_event_text("PreToolUse", &json!({"tool_name":"Bash","tool_input":{"command":"ls"}})),
            Some((EventKind::Tool, "● Bash(ls)".into()))
        );
        assert_eq!(
            hook_event_text("PreToolUse", &json!({"tool_name":"Glob"})),
            Some((EventKind::Tool, "● Glob()".into()))
        );
        assert_eq!(hook_event_text("PreToolUse", &json!({})), Some((EventKind::Tool, "● tool()".into())));
        assert_eq!(
            hook_event_text("Stop", &json!({"last_assistant_message":"done"})),
            Some((EventKind::Agent, "done".into()))
        );
        assert_eq!(hook_event_text("Stop", &json!({})), None);
        assert_eq!(
            hook_event_text("PermissionRequest", &json!({"tool_name":"Bash","tool_input":{"command":"rm x"}})),
            None,
            "the needs_you state change carries the permission line"
        );
        assert_eq!("tool".parse::<EventKind>().unwrap(), EventKind::Tool);
        assert!("nope".parse::<EventKind>().is_err());
        assert_eq!(EventKind::Agent.as_str(), "agent");
        assert_eq!(
            hook_event_text("Notification", &json!({"message":"hi"})),
            Some((EventKind::System, "hi".into()))
        );
        assert_eq!(
            hook_event_text("SessionEnd", &json!({"reason":"exit"})),
            Some((EventKind::System, "session ended (exit)".into()))
        );
        assert_eq!(hook_event_text("SessionEnd", &json!({})), Some((EventKind::System, "session ended".into())));
        assert_eq!(hook_event_text("PostToolUse", &json!({})), None);
    }

    #[test]
    fn client_frame_type_names_match_the_spec() {
        let v = serde_json::to_value(ClientFrame::FanoutNew {
            prompt: "p".into(),
            pearl_id: "x".into(),
            candidates: vec![],
            project: None,
        })
        .unwrap();
        assert_eq!(v["type"], "flow.fanout.new");
        let v = serde_json::to_value(ClientFrame::Approve {
            id: "a".into(),
            request_id: "r".into(),
            decision: Decision::AllowSession,
        })
        .unwrap();
        assert_eq!(v["decision"], "allow_session");
    }

    #[test]
    fn unknown_types_are_ignored_not_fatal() {
        assert!(parse_client_frame(r#"{"channel":"flow","type":"flow.future"}"#).unwrap().is_none());
        assert!(parse_client_frame(r#"{"type":"send_message"}"#).unwrap().is_none());
        assert!(parse_client_frame(r#"{"channel":"operator","type":"flow.attach","id":"x"}"#).unwrap().is_none());
        assert!(parse_client_frame(r#"{"channel":"flow"}"#).unwrap().is_none());
    }

    #[test]
    fn malformed_known_frame_is_an_error() {
        assert!(parse_client_frame(r#"{"type":"flow.attach","id":"x"}"#).is_err(), "missing cols/rows");
        assert!(parse_client_frame("not json").is_err());
        assert_eq!(client_seq(r#"{"seq":7,"type":"flow.snapshot"}"#), Some(json!(7)));
        assert_eq!(client_seq(r#"{"type":"flow.snapshot"}"#), None);
    }

    #[test]
    fn kill_resume_defaults_false() {
        let f = parse_client_frame(r#"{"type":"flow.kill","id":"x"}"#).unwrap().unwrap();
        assert_eq!(f, ClientFrame::Kill { id: "x".into(), resume: false });
    }

    #[test]
    fn server_frames_carry_channel_and_round_trip() {
        let frames = vec![
            ServerFrame::Hello {
                daemon: DaemonInfo {
                    version: "1".into(),
                    machine_label: "m".into(),
                },
                sessions: vec![session()],
                harnesses: vec![HarnessInfo {
                    name: "claude".into(),
                    display_name: "Claude Code".into(),
                    kind: "claude".into(),
                    installed: true,
                    binary_path: Some("/x/claude".into()),
                    state_source: "hooks".into(),
                    hidden: false,
                    order_index: 0,
                    reason: None,
                    origin: "builtin".into(),
                }],
            },
            ServerFrame::Harnesses { harnesses: vec![] },
            ServerFrame::Session { session: session() },
            ServerFrame::SessionRemoved { id: "x".into() },
            ServerFrame::Output {
                id: "x".into(),
                seq: 3,
                data_b64: "aGk=".into(),
            },
            ServerFrame::Screen {
                id: "x".into(),
                cols: 80,
                rows: 24,
                text: "$ ".into(),
            },
            ServerFrame::Attention {
                id: "x".into(),
                attention: Some(Attention::new("held")),
            },
            ServerFrame::Fanout {
                fan_out: FanOut {
                    id: "fo-1".into(),
                    prompt: "p".into(),
                    base_commit: "c".into(),
                    pearl_id: "th-1".into(),
                    created_at: Utc::now(),
                    winner_session_id: None,
                },
                candidates: vec![session()],
            },
            ServerFrame::error(Some(json!(1)), "not_found", "no such session"),
        ];
        for f in frames {
            let wire = f.to_wire();
            let v: Value = serde_json::from_str(&wire).unwrap();
            assert_eq!(v["channel"], "flow", "{wire}");
            assert!(v["type"].as_str().unwrap().starts_with("flow."));
            let back = parse_server_frame(&wire).unwrap();
            // Timestamps round-trip at rfc3339 precision — compare wire forms.
            assert_eq!(back.to_wire(), wire);
        }
    }

    #[test]
    fn error_frame_shape_is_an_object_with_ref() {
        let v: Value = serde_json::from_str(&ServerFrame::error(None, "bad_request", "nope").to_wire()).unwrap();
        assert_eq!(v["type"], "flow.error");
        assert!(v["ref"].is_null());
        assert_eq!(v["code"], "bad_request");
        assert_eq!(v["message"], "nope");
    }

    #[test]
    fn output_session_only_for_output() {
        let o = ServerFrame::Output {
            id: "x".into(),
            seq: 0,
            data_b64: String::new(),
        };
        assert_eq!(o.output_session(), Some("x"));
        assert_eq!(ServerFrame::SessionRemoved { id: "x".into() }.output_session(), None);
    }

    #[test]
    fn hook_mapping_table() {
        let empty = json!({});
        assert_eq!(map_hook_event("UserPromptSubmit", &empty), HookOutcome::Working);
        assert_eq!(map_hook_event("PreToolUse", &empty), HookOutcome::Working);
        assert_eq!(map_hook_event("PostToolUse", &empty), HookOutcome::Working);
        assert_eq!(map_hook_event("Stop", &empty), HookOutcome::Idle);
        assert_eq!(map_hook_event("SessionEnd", &empty), HookOutcome::Ended);
        assert_eq!(map_hook_event("SessionStart", &empty), HookOutcome::None);
        assert_eq!(map_hook_event("PreCompact", &empty), HookOutcome::None);
        assert_eq!(map_hook_event("SubagentStop", &empty), HookOutcome::None);
        assert_eq!(map_hook_event("Whatever", &empty), HookOutcome::None);

        let perm = json!({"tool_name":"Bash","tool_input":{"command":"rm -rf /tmp/x"}});
        match map_hook_event("PermissionRequest", &perm) {
            HookOutcome::NeedsYou(a) => {
                assert_eq!(a.reason, "permission");
                assert_eq!(a.detail.as_deref(), Some("Bash: rm -rf /tmp/x"));
            }
            other => panic!("{other:?}"),
        }
        let n = json!({"notification_type":"permission_prompt","message":"Claude needs your permission to use Bash"});
        assert!(matches!(map_hook_event("Notification", &n), HookOutcome::NeedsYou(a) if a.reason == "permission"));
        let n = json!({"notification_type":"idle_prompt","message":"Claude is waiting for your input"});
        assert!(matches!(map_hook_event("Notification", &n), HookOutcome::NeedsYou(a) if a.reason == "question"));
        let n = json!({"notification_type":"other","message":"fyi"});
        assert_eq!(map_hook_event("Notification", &n), HookOutcome::None);

        assert_eq!(outcome_state(&HookOutcome::Working), Some(SessionState::Working));
        assert_eq!(outcome_state(&HookOutcome::Idle), Some(SessionState::Idle));
        assert_eq!(outcome_state(&HookOutcome::NeedsYou(Attention::new("x"))), Some(SessionState::NeedsYou));
        assert_eq!(outcome_state(&HookOutcome::Ended), None);
        assert_eq!(outcome_state(&HookOutcome::None), None);
    }

    #[test]
    fn permission_detail_falls_back_sensibly() {
        assert_eq!(permission_detail(&json!({"tool_name":"Read","tool_input":{"file_path":"/a"}})), "Read: /a");
        assert_eq!(permission_detail(&json!({"tool_name":"Glob"})), "Glob");
        assert_eq!(permission_detail(&json!({})), "tool");
        let long = "x".repeat(500);
        let d = permission_detail(&json!({"tool_name":"Bash","tool_input":{"command":long}}));
        assert!(d.chars().count() <= 206);
        assert_eq!(permission_detail(&json!({"tool_name":"T","tool_input":{"k":1}})), r#"T: {"k":1}"#);
    }

    #[test]
    fn permission_reply_shapes() {
        let p = json!({"tool_name":"Bash"});
        let allow = permission_reply(Decision::Allow, &p);
        assert_eq!(allow["hookSpecificOutput"]["hookEventName"], "PermissionRequest");
        assert_eq!(allow["hookSpecificOutput"]["decision"]["behavior"], "allow");
        let deny = permission_reply(Decision::Deny, &p);
        assert_eq!(deny["hookSpecificOutput"]["decision"]["behavior"], "deny");
        assert!(deny["hookSpecificOutput"]["decision"]["message"].is_string());
        let sess = permission_reply(Decision::AllowSession, &p);
        assert_eq!(sess["hookSpecificOutput"]["decision"]["updatedPermissions"][0]["rules"][0]["toolName"], "Bash");
        assert_eq!(sess["hookSpecificOutput"]["decision"]["updatedPermissions"][0]["destination"], "session");
        let sess_no_tool = permission_reply(Decision::AllowSession, &json!({}));
        assert!(sess_no_tool["hookSpecificOutput"]["decision"]["updatedPermissions"].is_null());
    }

    #[test]
    fn approval_keystrokes() {
        assert_eq!(approval_keystroke(Decision::Allow), "1");
        assert_eq!(approval_keystroke(Decision::AllowSession), "2");
        assert_eq!(approval_keystroke(Decision::Deny), "Escape");
    }

    #[test]
    fn hook_event_body_parses_with_defaults() {
        let h: HookEvent = serde_json::from_str(r#"{"event":"Stop","session_id":"u"}"#).unwrap();
        assert_eq!(h.harness, "");
        assert!(h.cwd.is_none());
        assert!(h.payload.is_null());
        assert!(!is_flow_frame(&json!({"type":"x"})));
        assert!(is_flow_frame(&json!({"channel":"flow"})));
    }
}

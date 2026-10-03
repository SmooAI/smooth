//! The flow WS frames this client reads and writes (SmoothFlow.md § Frames).
//! Unknown frame types are ignored, by contract.

use base64::Engine as _;
use serde_json::{json, Value};
use smooth_flow_client::attention::Attention;
use smooth_flow_client::diff::Base;
use smooth_flow_client::harness::Harness;
use smooth_flow_client::Session;

use crate::diff::{base_str, Payload};

/// A session row plus the fields the shared `Session` doesn't carry.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub session: Session,
    pub attention: Option<Attention>,
}

impl Row {
    fn parse(v: &Value) -> Option<Self> {
        Some(Self {
            session: serde_json::from_value(v.clone()).ok()?,
            attention: v.get("attention").and_then(|a| serde_json::from_value(a.clone()).ok()),
        })
    }
}

fn harnesses(v: Option<&Value>) -> Option<Vec<Harness>> {
    v.and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|h| serde_json::from_value(h.clone()).ok()).collect())
}

/// What the engine told us.
#[derive(Debug, Clone, PartialEq)]
pub enum Inbound {
    /// `home` is the daemon's `$HOME` when it says (th-89eb13); `harnesses`
    /// is `None` from a daemon that predates the list.
    Hello {
        machine: String,
        home: Option<String>,
        sessions: Vec<Row>,
        harnesses: Option<Vec<Harness>>,
    },
    Session(Row),
    Removed(String),
    /// `flow.attention`: a session's attention changed (`None` clears it).
    Attention {
        id: String,
        attention: Option<Attention>,
    },
    /// `flow.harnesses`: the picker list changed; replaces it.
    Harnesses(Vec<Harness>),
    Output {
        id: String,
        seq: u64,
        bytes: Vec<u8>,
    },
    /// `flow.error`. `reference` is the `seq` of the client frame it
    /// answers, when that frame carried one (`ref` on the wire), so a
    /// refusal can be matched to the request that caused it.
    Error {
        reference: Option<u64>,
        /// `stale`, `blocked`, `not_found`, … when the engine says.
        code: Option<String>,
        message: String,
    },
    /// `flow.diff` (th-26f5b9): the reply to a diff request — never
    /// broadcast. `path` is set when it is one file's page.
    Diff {
        id: String,
        base: Base,
        path: Option<String>,
        diff: Box<Payload>,
    },
    /// `flow.diff.result`: a hunk action or a review went through.
    DiffResult {
        id: String,
        /// `revert` | `stage` | `unstage` | `review`.
        action: String,
        file: Option<String>,
    },
    /// `flow.diff.changed`: the session's diff may have moved; refetch.
    DiffChanged(String),
}

/// Parse one text frame; `None` for types this client doesn't use or a frame
/// it can't read.
#[must_use]
pub fn parse(text: &str) -> Option<Inbound> {
    let v: Value = serde_json::from_str(text).ok()?;
    match v.get("type")?.as_str()? {
        "flow.hello" => Some(Inbound::Hello {
            machine: v.pointer("/daemon/machine_label").and_then(Value::as_str).unwrap_or("").to_string(),
            home: v.pointer("/daemon/home").and_then(Value::as_str).filter(|h| !h.is_empty()).map(str::to_string),
            sessions: v
                .get("sessions")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(Row::parse).collect())
                .unwrap_or_default(),
            harnesses: harnesses(v.get("harnesses")),
        }),
        "flow.session" => Row::parse(v.get("session")?).map(Inbound::Session),
        "flow.attention" => Some(Inbound::Attention {
            id: v.get("id")?.as_str()?.to_string(),
            attention: v.get("attention").and_then(|a| serde_json::from_value(a.clone()).ok()),
        }),
        "flow.harnesses" => harnesses(v.get("harnesses")).map(Inbound::Harnesses),
        "flow.session.removed" => Some(Inbound::Removed(v.get("id")?.as_str()?.to_string())),
        "flow.output" => Some(Inbound::Output {
            id: v.get("id")?.as_str()?.to_string(),
            seq: v.get("seq").and_then(Value::as_u64).unwrap_or(0),
            bytes: base64::engine::general_purpose::STANDARD.decode(v.get("data_b64")?.as_str()?).ok()?,
        }),
        "flow.diff" => Some(Inbound::Diff {
            id: v.get("id")?.as_str()?.to_string(),
            base: serde_json::from_value(v.get("base")?.clone()).ok()?,
            path: v.get("path").and_then(Value::as_str).map(str::to_string),
            diff: Box::new(serde_json::from_value(v.get("diff")?.clone()).ok()?),
        }),
        "flow.diff.result" => Some(Inbound::DiffResult {
            id: v.get("id")?.as_str()?.to_string(),
            action: v.get("action")?.as_str()?.to_string(),
            file: v.get("file").and_then(Value::as_str).map(str::to_string),
        }),
        "flow.diff.changed" => Some(Inbound::DiffChanged(v.get("id")?.as_str()?.to_string())),
        "flow.error" => Some(Inbound::Error {
            reference: v.get("ref").and_then(Value::as_u64),
            code: v.pointer("/error/code").or_else(|| v.get("code")).and_then(Value::as_str).map(str::to_string),
            message: v
                .pointer("/error/message")
                .or_else(|| v.get("message"))
                .and_then(Value::as_str)
                .unwrap_or("error")
                .to_string(),
        }),
        _ => None,
    }
}

/// `flow.attach` — start streaming a session's PTY at this size.
#[must_use]
pub fn attach(id: &str, cols: u16, rows: u16) -> String {
    json!({ "type": "flow.attach", "id": id, "cols": cols, "rows": rows }).to_string()
}

/// `flow.detach`.
#[must_use]
pub fn detach(id: &str) -> String {
    json!({ "type": "flow.detach", "id": id }).to_string()
}

/// `flow.input` — keyboard / paste bytes.
#[must_use]
pub fn input(id: &str, bytes: &[u8]) -> String {
    json!({ "type": "flow.input", "id": id, "data_b64": base64::engine::general_purpose::STANDARD.encode(bytes) }).to_string()
}

/// `flow.resize`.
#[must_use]
pub fn resize(id: &str, cols: u16, rows: u16) -> String {
    json!({ "type": "flow.resize", "id": id, "cols": cols, "rows": rows }).to_string()
}

/// What `flow.new` carries (SmoothFlow.md § Frames). `None` fields are
/// left to the engine (it infers from the worktree).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NewSession {
    pub kind: String,
    pub worktree: Option<String>,
    pub project: Option<String>,
    pub pearl_id: Option<String>,
    pub prompt: Option<String>,
    pub title: Option<String>,
}

/// `flow.new` — start a session.
#[must_use]
pub fn new_session(n: &NewSession) -> String {
    let mut v = json!({ "type": "flow.new", "kind": n.kind });
    for (k, val) in [
        ("worktree", &n.worktree),
        ("project", &n.project),
        ("pearl_id", &n.pearl_id),
        ("prompt", &n.prompt),
        ("title", &n.title),
    ] {
        if let Some(x) = val.as_deref().map(str::trim).filter(|x| !x.is_empty()) {
            v[k] = json!(x);
        }
    }
    v.to_string()
}

/// A `flow.approve` decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Allow,
    Deny,
}

/// `flow.approve` — answer a permission request.
#[must_use]
pub fn approve(id: &str, request_id: &str, decision: Decision) -> String {
    let d = match decision {
        Decision::Allow => "allow",
        Decision::Deny => "deny",
    };
    json!({ "type": "flow.approve", "id": id, "request_id": request_id, "decision": d }).to_string()
}

/// `flow.kill` — stop a session (`resume` relaunches it).
#[must_use]
pub fn kill(id: &str, resume: bool) -> String {
    json!({ "type": "flow.kill", "id": id, "resume": resume }).to_string()
}

/// `flow.close` — Close Out: finish a session for good (SmoothFlow.md §
/// Frames, th-e126cc). The engine kills a live session first, closes its
/// pearl when `close_pearl`, and removes its worktree and branch when
/// `remove_worktree` and the branch is merged and clean. It refuses a dirty or
/// unmerged worktree with `flow.error` (`ref` = `seq`) and touches nothing.
/// `force` overrides that; this client sends it only from the Force close
/// button on a refusal, after the reason was read (spec §7).
#[must_use]
pub fn close(id: &str, close_pearl: bool, remove_worktree: bool, force: bool, seq: u64) -> String {
    json!({
        "type": "flow.close",
        "id": id,
        "close_pearl": close_pearl,
        "remove_worktree": remove_worktree,
        "force": force,
        "seq": seq,
    })
    .to_string()
}

/// `flow.diff` — ask for a session's diff against `base`; `path` asks for
/// one file's hunks (a collapsed or budget-stubbed file). `seq` comes back
/// as `ref` on a `flow.error`.
#[must_use]
pub fn diff(id: &str, base: Base, path: Option<&str>, seq: u64) -> String {
    let mut v = json!({ "type": "flow.diff", "id": id, "base": base_str(base), "seq": seq });
    if let Some(p) = path {
        v["path"] = json!(p);
    }
    v.to_string()
}

/// `flow.diff.revert` — reverse one hunk in the worktree. Refused with
/// `stale` when it no longer applies; never forced.
#[must_use]
pub fn diff_revert(id: &str, base: Base, hunk_id: &str, seq: u64) -> String {
    json!({ "type": "flow.diff.revert", "id": id, "base": base_str(base), "hunk_id": hunk_id, "seq": seq }).to_string()
}

/// `flow.diff.stage` (or `.unstage`) — one hunk of the uncommitted diff.
#[must_use]
pub fn diff_stage(id: &str, hunk_id: &str, unstage: bool, seq: u64) -> String {
    let t = if unstage { "flow.diff.unstage" } else { "flow.diff.stage" };
    json!({ "type": t, "id": id, "base": "uncommitted", "hunk_id": hunk_id, "seq": seq }).to_string()
}

/// `flow.diff.review` — the pending comments, as one steer to the agent.
#[must_use]
pub fn diff_review(id: &str, base: Base, comments: &[Value], seq: u64) -> String {
    json!({ "type": "flow.diff.review", "id": id, "base": base_str(base), "comments": comments, "seq": seq }).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use smooth_flow_client::SessionState;

    #[test]
    fn reads_what_it_uses_and_ignores_the_rest() {
        let hello = r#"{"type":"flow.hello","daemon":{"version":"1","machine_label":"marvin"},"sessions":[{"id":"fs-1","kind":"claude","state":"working","title":"t","argv":["claude"]}],"harnesses":[]}"#;
        let Some(Inbound::Hello {
            machine,
            home,
            sessions,
            harnesses,
        }) = parse(hello)
        else {
            panic!("hello")
        };
        assert_eq!(harnesses, Some(vec![]));
        assert_eq!(machine, "marvin");
        assert_eq!(home, None, "an older daemon says no home");
        let with_home = r#"{"type":"flow.hello","daemon":{"version":"1","machine_label":"m","home":"/home/me"},"sessions":[]}"#;
        let Some(Inbound::Hello { home, .. }) = parse(with_home) else {
            panic!("hello")
        };
        assert_eq!(home.as_deref(), Some("/home/me"));
        assert_eq!(sessions[0].session.state, SessionState::Working);
        let out = r#"{"type":"flow.output","id":"fs-1","seq":7,"data_b64":"aGk="}"#;
        assert_eq!(
            parse(out),
            Some(Inbound::Output {
                id: "fs-1".into(),
                seq: 7,
                bytes: b"hi".to_vec()
            })
        );
        assert_eq!(parse(r#"{"type":"flow.session.removed","id":"fs-2"}"#), Some(Inbound::Removed("fs-2".into())));
        assert_eq!(
            parse(r#"{"type":"flow.error","error":{"message":"nope"}}"#),
            Some(Inbound::Error {
                reference: None,
                code: None,
                message: "nope".into()
            })
        );
        assert_eq!(
            parse(r#"{"type":"flow.error","ref":7,"code":"error","message":"branch b is not merged"}"#),
            Some(Inbound::Error {
                reference: Some(7),
                code: Some("error".into()),
                message: "branch b is not merged".into()
            }),
            "the engine's flat error, with the request's seq echoed in ref"
        );
        assert_eq!(parse(r#"{"type":"flow.future_thing"}"#), None);
        assert_eq!(parse("not json"), None);
    }

    #[test]
    fn reads_harnesses_and_attention() {
        let hello = r#"{"type":"flow.hello","daemon":{"version":"1","machine_label":"m"},"sessions":[
            {"id":"fs-1","kind":"claude","state":"needs_you","attention":{"reason":"permission","detail":"rm -rf x","request_id":"r1"}}],
            "harnesses":[{"name":"claude","display_name":"Claude Code","kind":"claude","installed":true,"state_source":"hooks","order_index":0,"origin":"builtin",
              "health":{"verdict":"degraded","reason":"no login","fix":"claude login"}},
              {"name":"codex","display_name":"Codex","kind":"codex","installed":false,"reason":"not on PATH","state_source":"hooks","order_index":1,"origin":"builtin"}]}"#;
        let Some(Inbound::Hello { sessions, harnesses, .. }) = parse(hello) else {
            panic!("hello")
        };
        let a = sessions[0].attention.as_ref().map(|a| (a.reason.as_str(), a.request_id.as_deref()));
        assert_eq!(a, Some(("permission", Some("r1"))));
        let h = harnesses.unwrap_or_default();
        assert_eq!(h.len(), 2);
        assert!(h[0].is_degraded());
        assert_eq!(h[1].reason.as_deref(), Some("not on PATH"));
        let older = r#"{"type":"flow.hello","daemon":{"version":"1","machine_label":"m"},"sessions":[]}"#;
        assert!(matches!(parse(older), Some(Inbound::Hello { harnesses: None, .. })));
        let Some(Inbound::Harnesses(h)) = parse(r#"{"type":"flow.harnesses","harnesses":[{"name":"x"}]}"#) else {
            panic!("harnesses")
        };
        assert_eq!((h[0].name.as_str(), h[0].installed), ("x", false), "installed defaults to false, like the Mac");
        let att = parse(r#"{"type":"flow.attention","id":"fs-1","attention":{"reason":"question","request_id":42}}"#);
        let Some(Inbound::Attention { id, attention }) = att else {
            panic!("attention")
        };
        assert_eq!((id.as_str(), attention.and_then(|a| a.request_id).as_deref()), ("fs-1", Some("42")));
        assert!(matches!(
            parse(r#"{"type":"flow.attention","id":"fs-1","attention":null}"#),
            Some(Inbound::Attention { attention: None, .. })
        ));
    }

    #[test]
    fn writes_new_approve_and_kill() {
        let n = NewSession {
            kind: "claude".into(),
            worktree: Some("/w/x".into()),
            prompt: Some("  ".into()),
            ..NewSession::default()
        };
        let v: Value = serde_json::from_str(&new_session(&n)).unwrap_or_default();
        assert_eq!(
            v,
            json!({ "type": "flow.new", "kind": "claude", "worktree": "/w/x" }),
            "blank fields stay off the wire"
        );
        let v: Value = serde_json::from_str(&approve("fs-1", "r1", Decision::Deny)).unwrap_or_default();
        assert_eq!(v, json!({ "type": "flow.approve", "id": "fs-1", "request_id": "r1", "decision": "deny" }));
        let v: Value = serde_json::from_str(&kill("fs-1", false)).unwrap_or_default();
        assert_eq!(v["type"], "flow.kill");
        let v: Value = serde_json::from_str(&close("fs-1", true, false, false, 3)).unwrap_or_default();
        assert_eq!(
            v,
            json!({ "type": "flow.close", "id": "fs-1", "close_pearl": true, "remove_worktree": false, "force": false, "seq": 3 }),
        );
        let v: Value = serde_json::from_str(&close("fs-1", false, true, true, 4)).unwrap_or_default();
        assert_eq!((v["force"].as_bool(), v["seq"].as_u64()), (Some(true), Some(4)));
    }

    #[test]
    fn reads_and_writes_the_diff_frames() {
        let d = r#"{"channel":"flow","type":"flow.diff","id":"fs-1","base":"uncommitted","path":"a.rs",
            "diff":{"base":"uncommitted","from":{"ref":"HEAD","label":"HEAD"},"to":{"ref":"","label":"worktree"},
            "files":[{"path":"a.rs","status":"added","added":1,"deleted":0,"hunks":[{"id":"h","old_start":0,"old_lines":0,"new_start":1,"new_lines":1,
            "lines":[{"kind":"add","new":1,"text":"x"}]}]}],"added":1,"deleted":0,"legend":[]}}"#;
        let Some(Inbound::Diff { id, base, path, diff: payload }) = parse(d) else {
            panic!("diff")
        };
        assert_eq!((id.as_str(), base, path.as_deref()), ("fs-1", Base::Uncommitted, Some("a.rs")));
        assert_eq!(payload.files[0].hunks[0].lines[0].new, Some(1));
        assert_eq!(
            parse(r#"{"type":"flow.diff.result","id":"fs-1","action":"revert","hunk_id":"h","file":"a.rs"}"#),
            Some(Inbound::DiffResult {
                id: "fs-1".into(),
                action: "revert".into(),
                file: Some("a.rs".into())
            })
        );
        assert_eq!(parse(r#"{"type":"flow.diff.changed","id":"fs-1"}"#), Some(Inbound::DiffChanged("fs-1".into())));
        assert_eq!(
            parse(r#"{"type":"flow.diff","id":"fs-1","base":"main","diff":{}}"#),
            None,
            "an unknown base is unreadable"
        );
        let stale = parse(r#"{"type":"flow.error","ref":4294967296,"code":"stale","message":"stale: gone"}"#);
        assert!(matches!(stale, Some(Inbound::Error { reference: Some(4_294_967_296), code: Some(ref c), .. }) if c == "stale"));

        let v: Value = serde_json::from_str(&diff("fs-1", Base::Turn, None, 9)).unwrap_or_default();
        assert_eq!(v, json!({ "type": "flow.diff", "id": "fs-1", "base": "turn", "seq": 9 }));
        let v: Value = serde_json::from_str(&diff("fs-1", Base::Branch, Some("Cargo.lock"), 1)).unwrap_or_default();
        assert_eq!(v["path"], "Cargo.lock");
        let v: Value = serde_json::from_str(&diff_revert("fs-1", Base::Turn, "h", 2)).unwrap_or_default();
        assert_eq!(v, json!({ "type": "flow.diff.revert", "id": "fs-1", "base": "turn", "hunk_id": "h", "seq": 2 }));
        let v: Value = serde_json::from_str(&diff_stage("fs-1", "h", true, 3)).unwrap_or_default();
        assert_eq!(v["type"], "flow.diff.unstage");
        let v: Value = serde_json::from_str(&diff_review("fs-1", Base::Turn, &[json!({"file":"a","text":"t"})], 4)).unwrap_or_default();
        assert_eq!(v["comments"][0]["text"], "t");
    }

    #[test]
    fn writes_the_client_frames() {
        let v: Value = serde_json::from_str(&input("fs-1", b"ls\r")).unwrap_or_default();
        assert_eq!(v["type"], "flow.input");
        assert_eq!(v["data_b64"], "bHMN");
        let v: Value = serde_json::from_str(&attach("fs-1", 80, 24)).unwrap_or_default();
        assert_eq!((v["cols"].as_u64(), v["rows"].as_u64()), (Some(80), Some(24)));
    }
}

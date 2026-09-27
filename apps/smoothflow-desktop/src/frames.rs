//! The flow WS frames this client reads and writes (SmoothFlow.md § Frames).
//! Unknown frame types are ignored, by contract.

use base64::Engine as _;
use serde_json::{json, Value};
use smooth_flow_client::Session;

/// What the engine told us.
#[derive(Debug, Clone, PartialEq)]
pub enum Inbound {
    Hello { machine: String, sessions: Vec<Session> },
    Session(Session),
    Removed(String),
    Output { id: String, seq: u64, bytes: Vec<u8> },
    Error(String),
}

/// Parse one text frame; `None` for types this client doesn't use or a frame
/// it can't read.
#[must_use]
pub fn parse(text: &str) -> Option<Inbound> {
    let v: Value = serde_json::from_str(text).ok()?;
    match v.get("type")?.as_str()? {
        "flow.hello" => Some(Inbound::Hello {
            machine: v.pointer("/daemon/machine_label").and_then(Value::as_str).unwrap_or("").to_string(),
            sessions: v
                .get("sessions")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(|s| serde_json::from_value(s.clone()).ok()).collect())
                .unwrap_or_default(),
        }),
        "flow.session" => serde_json::from_value(v.get("session")?.clone()).ok().map(Inbound::Session),
        "flow.session.removed" => Some(Inbound::Removed(v.get("id")?.as_str()?.to_string())),
        "flow.output" => Some(Inbound::Output {
            id: v.get("id")?.as_str()?.to_string(),
            seq: v.get("seq").and_then(Value::as_u64).unwrap_or(0),
            bytes: base64::engine::general_purpose::STANDARD.decode(v.get("data_b64")?.as_str()?).ok()?,
        }),
        "flow.error" => Some(Inbound::Error(
            v.pointer("/error/message")
                .or_else(|| v.get("message"))
                .and_then(Value::as_str)
                .unwrap_or("error")
                .to_string(),
        )),
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

#[cfg(test)]
mod tests {
    use super::*;
    use smooth_flow_client::SessionState;

    #[test]
    fn reads_what_it_uses_and_ignores_the_rest() {
        let hello = r#"{"type":"flow.hello","daemon":{"version":"1","machine_label":"marvin"},"sessions":[{"id":"fs-1","kind":"claude","state":"working","title":"t","argv":["claude"]}],"harnesses":[]}"#;
        let Some(Inbound::Hello { machine, sessions }) = parse(hello) else { panic!("hello") };
        assert_eq!(machine, "marvin");
        assert_eq!(sessions[0].state, SessionState::Working);
        let out = r#"{"type":"flow.output","id":"fs-1","seq":7,"data_b64":"aGk="}"#;
        assert_eq!(parse(out), Some(Inbound::Output { id: "fs-1".into(), seq: 7, bytes: b"hi".to_vec() }));
        assert_eq!(parse(r#"{"type":"flow.session.removed","id":"fs-2"}"#), Some(Inbound::Removed("fs-2".into())));
        assert_eq!(parse(r#"{"type":"flow.error","error":{"message":"nope"}}"#), Some(Inbound::Error("nope".into())));
        assert_eq!(parse(r#"{"type":"flow.future_thing"}"#), None);
        assert_eq!(parse("not json"), None);
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

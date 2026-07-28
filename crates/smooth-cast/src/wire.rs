//! Canonical-protocol frame helpers shared by every Smooth WebSocket client.
//!
//! `th code` (`smooth-code`), `th api smooth-operator` (`smooth-cli`) and the
//! bench driver (`smooth-bench`) all speak the same canonical operator
//! protocol, and each had hand-rolled its own field lookups. They each got the
//! `error` frame wrong in a different way, so a real server complaint reached
//! users as the literal string `"unknown error"` (pearl th-472012).
//!
//! This module is the one place that knows the wire shapes.

use serde_json::Value;

/// Pull the human-readable text out of a canonical `error` frame.
///
/// The server builds these in `smooth-operator-server`'s `protocol::error` as:
///
/// ```json
/// { "type": "error",
///   "error": { "code": "VALIDATION_ERROR", "message": "missing 'action' field" },
///   "data":  { "error": { "code": "…", "message": "…" } },
///   "timestamp": 1234 }
/// ```
///
/// so `error` is an **object**. Reading it with `as_str()` yields `None`, which
/// is exactly how the real reason got dropped on the floor at three separate
/// call sites. Older/other engines have also used `/data/message` and a
/// top-level `message`, so all four shapes are checked, most specific first.
///
/// The `code` is prefixed when present — `"VALIDATION_ERROR: missing 'action'
/// field"` tells you what to do; `"unknown error"` does not.
#[must_use]
pub fn error_message(v: &Value) -> String {
    let text = v
        .pointer("/error/message")
        .or_else(|| v.pointer("/data/error/message"))
        .or_else(|| v.pointer("/data/message"))
        .or_else(|| v.get("message"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or("unknown error");

    match v
        .pointer("/error/code")
        .or_else(|| v.pointer("/data/error/code"))
        .and_then(Value::as_str)
        .filter(|c| !c.is_empty())
    {
        Some(code) => format!("{code}: {text}"),
        None => text.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::error_message;
    use serde_json::{json, Value};

    /// The exact frame `protocol::error` emits — and the exact frame that used
    /// to render as "unknown error" while killing a turn every 15 seconds.
    #[test]
    fn reads_the_real_server_error_frame() {
        let frame = json!({
            "type": "error",
            "error": { "code": "VALIDATION_ERROR", "message": "missing 'action' field" },
            "data": { "error": { "code": "VALIDATION_ERROR", "message": "missing 'action' field" } },
            "timestamp": 1_769_000_000_000_u64,
        });
        assert_eq!(error_message(&frame), "VALIDATION_ERROR: missing 'action' field");
    }

    #[test]
    fn falls_back_through_every_legacy_shape() {
        assert_eq!(error_message(&json!({ "data": { "error": { "message": "nested" } } })), "nested");
        assert_eq!(error_message(&json!({ "data": { "message": "data-level" } })), "data-level");
        assert_eq!(error_message(&json!({ "message": "top-level" })), "top-level");
    }

    #[test]
    fn code_is_optional() {
        assert_eq!(error_message(&json!({ "error": { "message": "no code here" } })), "no code here");
    }

    /// An `error` object with no readable message must not silently become the
    /// empty string — the caller is about to show this to a human.
    #[test]
    fn degrades_to_a_named_unknown() {
        assert_eq!(error_message(&json!({ "type": "error", "error": {} })), "unknown error");
        assert_eq!(error_message(&json!({ "type": "error" })), "unknown error");
        assert_eq!(error_message(&json!({ "message": "" })), "unknown error");
    }

    /// Regression: `error` is an object, so a bare `as_str()` on it returns
    /// `None`. Guard the specific mistake this module exists to prevent.
    #[test]
    fn object_valued_error_is_not_stringified_as_unknown() {
        let frame = json!({ "error": { "code": "RATE_LIMITED", "message": "slow down" } });
        assert!(frame.get("error").and_then(Value::as_str).is_none(), "precondition: `error` is an object");
        assert_eq!(error_message(&frame), "RATE_LIMITED: slow down");
    }
}

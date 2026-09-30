//! Why a session wants you, and whether a client may answer it (spec §7).

use serde::{Deserialize, Deserializer, Serialize};

/// A session's `attention` (the `flow.session` / `flow.attention` payload).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Attention {
    /// `permission` | `question` | `usage_limit` | `crashed` | `held` | anything newer.
    #[serde(default)]
    pub reason: String,
    /// The command or question being asked, for `permission` / `question`.
    #[serde(default, deserialize_with = "flexible_string", skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default, deserialize_with = "flexible_string", skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume_at: Option<String>,
}

/// A string, or a number sent as one (older engines), or null.
fn flexible_string<'de, D: Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    let v = Option::<serde_json::Value>::deserialize(d)?;
    Ok(match v {
        Some(serde_json::Value::String(s)) => Some(s),
        Some(serde_json::Value::Number(n)) => Some(n.to_string()),
        _ => None,
    })
}

/// An answerable request: what to show and what `flow.approve` carries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Approval {
    pub request_id: String,
    /// "Permission request" or "Question".
    pub heading: String,
    /// The command or question; never blank.
    pub text: String,
}

/// The approval a client may offer, if any. Only `permission` and `question`
/// attentions carrying a non-blank `request_id` are approvable; anything else
/// is a card without Allow/Deny.
#[must_use]
pub fn approval(attention: Option<&Attention>) -> Option<Approval> {
    let a = attention?;
    let heading = match a.reason.as_str() {
        "permission" => "Permission request",
        "question" => "Question",
        _ => return None,
    };
    let request_id = a.request_id.as_deref().map(str::trim).filter(|r| !r.is_empty())?;
    let text = a.detail.as_deref().map(str::trim).filter(|d| !d.is_empty()).map_or_else(
        || {
            if a.reason == "permission" {
                "The session wants permission."
            } else {
                "The session has a question."
            }
            .to_string()
        },
        str::to_string,
    );
    Some(Approval {
        request_id: request_id.to_string(),
        heading: heading.to_string(),
        text,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a(json: &str) -> Attention {
        serde_json::from_str(json).unwrap_or_default()
    }

    #[test]
    fn only_permission_or_question_with_a_request_id_is_approvable() {
        let p = approval(Some(&a(r#"{"reason":"permission","detail":"rm -rf target","request_id":"r1"}"#)));
        assert_eq!(
            p.map(|p| (p.heading, p.text, p.request_id)),
            Some(("Permission request".into(), "rm -rf target".into(), "r1".into()))
        );
        let q = approval(Some(&a(r#"{"reason":"question","request_id":7}"#)));
        assert_eq!(q.map(|q| (q.request_id, q.text)), Some(("7".into(), "The session has a question.".into())));
        assert_eq!(approval(Some(&a(r#"{"reason":"permission","detail":"x"}"#))), None, "no request id");
        assert_eq!(approval(Some(&a(r#"{"reason":"permission","request_id":"  "}"#))), None, "blank request id");
        assert_eq!(approval(Some(&a(r#"{"reason":"usage_limit","request_id":"r"}"#))), None);
        assert_eq!(approval(None), None);
    }
}

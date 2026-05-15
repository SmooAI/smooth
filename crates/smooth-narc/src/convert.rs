//! Conversions between the in-crate Rust types and the proto-generated
//! wire types. Centralised here so a proto-shape change has one place
//! to update.
//!
//! Pearl th-893801.

use crate::judge::{Decision, JudgeDecision, JudgeKind, JudgeRequest, Scope};
use crate::pb;

// --- JudgeKind ---

impl From<JudgeKind> for pb::JudgeKind {
    fn from(k: JudgeKind) -> Self {
        match k {
            JudgeKind::Network => pb::JudgeKind::Network,
            JudgeKind::Tool => pb::JudgeKind::Tool,
            JudgeKind::File => pb::JudgeKind::File,
            JudgeKind::Cli => pb::JudgeKind::Cli,
            JudgeKind::Mcp => pb::JudgeKind::Mcp,
            JudgeKind::Port => pb::JudgeKind::Port,
        }
    }
}

impl TryFrom<pb::JudgeKind> for JudgeKind {
    type Error = &'static str;
    fn try_from(k: pb::JudgeKind) -> Result<Self, Self::Error> {
        match k {
            pb::JudgeKind::Unspecified => Err("JudgeKind::Unspecified"),
            pb::JudgeKind::Network => Ok(JudgeKind::Network),
            pb::JudgeKind::Tool => Ok(JudgeKind::Tool),
            pb::JudgeKind::File => Ok(JudgeKind::File),
            pb::JudgeKind::Cli => Ok(JudgeKind::Cli),
            pb::JudgeKind::Mcp => Ok(JudgeKind::Mcp),
            pb::JudgeKind::Port => Ok(JudgeKind::Port),
        }
    }
}

// --- Scope ---

impl From<Scope> for pb::Scope {
    fn from(s: Scope) -> Self {
        match s {
            Scope::Once => pb::Scope::Once,
            Scope::Session => pb::Scope::Session,
            Scope::PearlProject => pb::Scope::PearlProject,
            Scope::User => pb::Scope::User,
        }
    }
}

impl TryFrom<pb::Scope> for Scope {
    type Error = &'static str;
    fn try_from(s: pb::Scope) -> Result<Self, Self::Error> {
        match s {
            pb::Scope::Unspecified => Err("Scope::Unspecified"),
            pb::Scope::Once => Ok(Scope::Once),
            pb::Scope::Session => Ok(Scope::Session),
            pb::Scope::PearlProject => Ok(Scope::PearlProject),
            pb::Scope::User => Ok(Scope::User),
        }
    }
}

// --- Decision ---

impl From<Decision> for pb::Decision {
    fn from(d: Decision) -> Self {
        match d {
            Decision::Approve => pb::Decision::Approve,
            Decision::Deny => pb::Decision::Deny,
            Decision::Ask => pb::Decision::Ask,
            Decision::EscalateToHuman => pb::Decision::EscalateToHuman,
        }
    }
}

impl TryFrom<pb::Decision> for Decision {
    type Error = &'static str;
    fn try_from(d: pb::Decision) -> Result<Self, Self::Error> {
        match d {
            pb::Decision::Unspecified => Err("Decision::Unspecified"),
            pb::Decision::Approve => Ok(Decision::Approve),
            pb::Decision::Deny => Ok(Decision::Deny),
            pb::Decision::Ask => Ok(Decision::Ask),
            pb::Decision::EscalateToHuman => Ok(Decision::EscalateToHuman),
        }
    }
}

// --- JudgeRequest ---

impl From<JudgeRequest> for pb::JudgeRequest {
    fn from(r: JudgeRequest) -> Self {
        let kind: pb::JudgeKind = r.kind.into();
        pb::JudgeRequest {
            kind: kind as i32,
            operator_id: r.operator_id,
            bead_id: r.bead_id,
            phase: r.phase,
            resource: r.resource,
            detail: r.detail.unwrap_or_default(),
            task_summary: r.task_summary.unwrap_or_default(),
            agent_reason: r.agent_reason.unwrap_or_default(),
        }
    }
}

impl TryFrom<pb::JudgeRequest> for JudgeRequest {
    type Error = String;
    fn try_from(r: pb::JudgeRequest) -> Result<Self, Self::Error> {
        let kind: JudgeKind = pb::JudgeKind::try_from(r.kind)
            .map_err(|_| format!("unknown JudgeKind enum value {}", r.kind))?
            .try_into()
            .map_err(|e: &str| e.to_string())?;
        Ok(JudgeRequest {
            kind,
            operator_id: r.operator_id,
            bead_id: r.bead_id,
            phase: r.phase,
            resource: r.resource,
            // Empty strings come back as None to preserve the
            // Option semantics on the Rust side; protobuf doesn't
            // distinguish unset-string from empty-string.
            detail: if r.detail.is_empty() { None } else { Some(r.detail) },
            task_summary: if r.task_summary.is_empty() { None } else { Some(r.task_summary) },
            agent_reason: if r.agent_reason.is_empty() { None } else { Some(r.agent_reason) },
        })
    }
}

// --- JudgeDecision ---

impl From<JudgeDecision> for pb::JudgeDecision {
    fn from(d: JudgeDecision) -> Self {
        let decision: pb::Decision = d.decision.into();
        // The proto carries resolved_scope, but the in-crate
        // JudgeDecision doesn't have one today — it lives implicitly
        // in the caller's resolution path. Default to UNSPECIFIED
        // (zero value); upgrade when we plumb resolved scope through.
        pb::JudgeDecision {
            decision: decision as i32,
            confidence: d.confidence,
            reason: d.reason,
            add_to_allowlist_glob: d.add_to_allowlist_glob.unwrap_or_default(),
            cache_ttl_seconds: d.cache_ttl_seconds.unwrap_or(0),
            resolved_scope: pb::Scope::Unspecified as i32,
        }
    }
}

impl TryFrom<pb::JudgeDecision> for JudgeDecision {
    type Error = String;
    fn try_from(d: pb::JudgeDecision) -> Result<Self, Self::Error> {
        let decision: Decision = pb::Decision::try_from(d.decision)
            .map_err(|_| format!("unknown Decision enum value {}", d.decision))?
            .try_into()
            .map_err(|e: &str| e.to_string())?;
        Ok(JudgeDecision {
            decision,
            confidence: d.confidence,
            reason: d.reason,
            add_to_allowlist_glob: if d.add_to_allowlist_glob.is_empty() {
                None
            } else {
                Some(d.add_to_allowlist_glob)
            },
            cache_ttl_seconds: if d.cache_ttl_seconds == 0 { None } else { Some(d.cache_ttl_seconds) },
            // Pearl th-49b4aa shipped JudgeDecision with scope_options;
            // wire it through once we extend the proto. For now,
            // default to empty.
            scope_options: Vec::new(),
        })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::judge::{Decision, JudgeKind, Scope};

    #[test]
    fn judge_kind_round_trips() {
        for k in [
            JudgeKind::Network,
            JudgeKind::Tool,
            JudgeKind::File,
            JudgeKind::Cli,
            JudgeKind::Mcp,
            JudgeKind::Port,
        ] {
            let pb_k: pb::JudgeKind = k.into();
            let back: JudgeKind = pb_k.try_into().unwrap();
            assert_eq!(k, back, "round-trip {k:?}");
        }
    }

    #[test]
    fn judge_kind_unspecified_fails() {
        let r: Result<JudgeKind, &str> = pb::JudgeKind::Unspecified.try_into();
        assert!(r.is_err());
    }

    #[test]
    fn decision_round_trips() {
        for d in [Decision::Approve, Decision::Deny, Decision::Ask, Decision::EscalateToHuman] {
            let pb_d: pb::Decision = d.into();
            let back: Decision = pb_d.try_into().unwrap();
            assert_eq!(d, back);
        }
    }

    #[test]
    fn scope_round_trips() {
        for s in Scope::default_options() {
            let pb_s: pb::Scope = s.into();
            let back: Scope = pb_s.try_into().unwrap();
            assert_eq!(s, back);
        }
    }

    #[test]
    fn judge_request_round_trips_with_optional_fields() {
        let req = JudgeRequest {
            kind: JudgeKind::Network,
            operator_id: "op-1".into(),
            bead_id: "pearl-1".into(),
            phase: "execute".into(),
            resource: "registry.npmjs.org".into(),
            detail: Some("/foo".into()),
            task_summary: Some("npm install".into()),
            agent_reason: None,
        };
        let pb_req: pb::JudgeRequest = req.clone().into();
        let back: JudgeRequest = pb_req.try_into().unwrap();
        assert_eq!(back.kind, req.kind);
        assert_eq!(back.resource, req.resource);
        assert_eq!(back.detail, req.detail);
        assert_eq!(back.task_summary, req.task_summary);
        // None survives as None (empty string in proto, mapped back to None).
        assert_eq!(back.agent_reason, None);
    }

    #[test]
    fn judge_decision_round_trips() {
        let d = JudgeDecision::approve("ok");
        let pb_d: pb::JudgeDecision = d.clone().into();
        let back: JudgeDecision = pb_d.try_into().unwrap();
        assert_eq!(back.decision, d.decision);
        assert!((back.confidence - d.confidence).abs() < 1e-6);
        assert_eq!(back.reason, d.reason);
        // Approve helper sets cache_ttl_seconds=Some(3600).
        assert_eq!(back.cache_ttl_seconds, d.cache_ttl_seconds);
    }
}

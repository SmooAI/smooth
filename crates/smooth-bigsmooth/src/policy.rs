//! Policy generation for operator sandboxes.
//!
//! Big Smooth generates a TOML policy for each operator based on:
//! - The current orchestration phase
//! - The assigned bead and its dependencies
//! - Operator-specific token

use chrono::Utc;
use smooth_policy::{AccessRequestConfig, AuthConfig, BeadsPolicy, FilesystemPolicy, McpPolicy, NetworkPolicy, Policy, PolicyMetadata, ToolsPolicy};

/// Generate a complete policy for an operator.
///
/// # Errors
/// Returns error if the policy cannot be serialized.
pub fn generate_policy(operator_id: &str, bead_id: &str, phase: &str, token: &str, bead_deps: &[String]) -> anyhow::Result<String> {
    let policy = Policy {
        metadata: PolicyMetadata {
            operator_id: operator_id.to_string(),
            bead_id: bead_id.to_string(),
            phase: phase.to_string(),
            generated_at: Some(Utc::now()),
        },
        auth: AuthConfig {
            token: token.to_string(),
            leader_url: "http://host.containers.internal:4400".to_string(),
        },
        network: NetworkPolicy {
            allow: smooth_policy::phase_network_defaults(phase),
            max_response_bytes: 52_428_800,
            leader: Default::default(),
        },
        beads: beads_policy(bead_id, bead_deps, phase),
        filesystem: filesystem_policy(phase),
        tools: tools_policy(phase),
        mcp: McpPolicy {
            allow_servers: vec!["smooth-tools".into()],
            deny_unknown_servers: true,
            allow_server_install: false,
        },
        access_requests: AccessRequestConfig {
            enabled: true,
            auto_approve_domains: vec!["*.npmjs.org".into(), "*.pypi.org".into(), "*.crates.io".into()],
            auto_approve_tools: vec!["lint_fix".into(), "test_run".into()],
        },
    };

    Ok(policy.to_toml()?)
}

fn beads_policy(bead_id: &str, deps: &[String], phase: &str) -> BeadsPolicy {
    let mut accessible = vec![bead_id.to_string()];
    accessible.extend(deps.iter().cloned());

    BeadsPolicy {
        accessible,
        include_dependencies: true,
        max_depth: smooth_policy::phase_beads_depth(phase),
    }
}

fn filesystem_policy(phase: &str) -> FilesystemPolicy {
    FilesystemPolicy {
        deny_patterns: vec![
            "*.env".into(),
            "*.pem".into(),
            "*.key".into(),
            "credentials.*".into(),
            ".ssh/*".into(),
            ".aws/*".into(),
            ".smooth/providers.json".into(),
            ".git/config".into(),
        ],
        writable: smooth_policy::phase_filesystem_writable(phase),
    }
}

fn tools_policy(phase: &str) -> ToolsPolicy {
    let mut allow = vec![
        "beads_context".into(),
        "beads_message".into(),
        "progress".into(),
        "code_search".into(),
        "find_definition".into(),
        "repo_map".into(),
    ];

    // Extra tools in execute/finalize phases
    if matches!(phase, "execute" | "finalize") {
        allow.extend(["artifact_write".into(), "lint_fix".into(), "test_run".into(), "spawn_subtask".into()]);
    }

    ToolsPolicy {
        allow,
        deny: vec!["workflow".into()],
    }
}

/// Generate an operator auth token.
pub fn generate_operator_token(operator_id: &str) -> String {
    let random_hex: String = (0..16).map(|_| format!("{:x}", rand_byte())).collect();
    format!("smth_op_{operator_id}_{random_hex}")
}

fn rand_byte() -> u8 {
    // Simple random byte using timestamp + thread id mixing
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    let tid = std::thread::current().id();
    let hash = format!("{t:?}{tid:?}");
    hash.as_bytes().iter().fold(0u8, |acc, &b| acc.wrapping_add(b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_assess_policy() {
        let toml = generate_policy("op-1", "smooth-abc", "assess", "smth_op_token", &[]).expect("generate");
        let policy = smooth_policy::Policy::from_toml(&toml).expect("parse");

        assert_eq!(policy.metadata.operator_id, "op-1");
        assert_eq!(policy.metadata.bead_id, "smooth-abc");
        assert_eq!(policy.metadata.phase, "assess");
        assert!(!policy.filesystem.writable); // assess = read-only
        assert_eq!(policy.beads.max_depth, 1); // assess = depth 1
        assert!(policy.network.is_allowed("opencode.ai", "/"));
        assert!(!policy.network.is_allowed("api.github.com", "/repos/SmooAI/smooth"));
        // no GitHub in assess
    }

    #[test]
    fn generate_execute_policy() {
        let toml = generate_policy("op-2", "smooth-xyz", "execute", "smth_op_token2", &["smooth-dep1".into()]).expect("generate");
        let policy = smooth_policy::Policy::from_toml(&toml).expect("parse");

        assert!(policy.filesystem.writable); // execute = writable
        assert_eq!(policy.beads.max_depth, 2);
        assert!(policy.beads.can_access("smooth-xyz"));
        assert!(policy.beads.can_access("smooth-dep1"));
        assert!(!policy.beads.can_access("smooth-other"));
        assert!(policy.network.is_allowed("api.github.com", "/repos/SmooAI/smooth")); // GitHub in execute
        assert!(policy.tools.can_use("artifact_write")); // execute has write tools
        assert!(policy.tools.can_use("lint_fix"));
    }

    #[test]
    fn generate_review_policy() {
        let toml = generate_policy("op-3", "smooth-rev", "review", "smth_op_token3", &["smooth-target".into()]).expect("generate");
        let policy = smooth_policy::Policy::from_toml(&toml).expect("parse");

        assert!(!policy.filesystem.writable); // review = read-only
        assert!(!policy.tools.can_use("artifact_write")); // review has no write tools
        assert!(policy.tools.can_use("code_search")); // but can search
    }

    #[test]
    fn policy_roundtrip() {
        let toml = generate_policy("op-1", "bead-1", "execute", "token", &[]).expect("generate");
        let policy = smooth_policy::Policy::from_toml(&toml).expect("parse");
        let toml2 = policy.to_toml().expect("re-serialize");
        let policy2 = smooth_policy::Policy::from_toml(&toml2).expect("re-parse");
        assert_eq!(policy2.metadata.operator_id, "op-1");
    }

    #[test]
    fn filesystem_deny_patterns_present() {
        let toml = generate_policy("op-1", "bead-1", "execute", "token", &[]).expect("generate");
        let policy = smooth_policy::Policy::from_toml(&toml).expect("parse");
        assert!(policy.filesystem.is_denied(".env").expect("glob"));
        assert!(policy.filesystem.is_denied("secret.pem").expect("glob"));
        assert!(policy.filesystem.is_denied(".ssh/id_rsa").expect("glob"));
        assert!(!policy.filesystem.is_denied("src/main.rs").expect("glob"));
    }

    #[test]
    fn mcp_defaults() {
        let toml = generate_policy("op-1", "bead-1", "execute", "token", &[]).expect("generate");
        let policy = smooth_policy::Policy::from_toml(&toml).expect("parse");
        assert!(policy.mcp.can_connect("smooth-tools"));
        assert!(!policy.mcp.can_connect("random-server"));
        assert!(!policy.mcp.allow_server_install);
    }

    #[test]
    fn access_request_auto_approve() {
        let toml = generate_policy("op-1", "bead-1", "execute", "token", &[]).expect("generate");
        let policy = smooth_policy::Policy::from_toml(&toml).expect("parse");
        assert!(policy.access_requests.should_auto_approve_domain("registry.npmjs.org"));
        assert!(policy.access_requests.should_auto_approve_tool("lint_fix"));
        assert!(!policy.access_requests.should_auto_approve_domain("api.stripe.com"));
    }

    #[test]
    fn operator_token_format() {
        let token = generate_operator_token("op-abc123");
        assert!(token.starts_with("smth_op_op-abc123_"));
        assert!(token.len() > 20);
    }

    #[test]
    fn workflow_always_denied() {
        for phase in &["assess", "plan", "orchestrate", "execute", "finalize", "review"] {
            let toml = generate_policy("op", "bead", phase, "tok", &[]).expect("generate");
            let policy = smooth_policy::Policy::from_toml(&toml).expect("parse");
            assert!(!policy.tools.can_use("workflow"), "workflow should be denied in {phase}");
        }
    }
}

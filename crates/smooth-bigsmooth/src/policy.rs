//! Policy generation for operator sandboxes.
//!
//! Big Smooth generates a TOML policy for each operator based on:
//! - The current orchestration phase
//! - The assigned bead and its dependencies
//! - Operator-specific token
//! - Optional task type (coding, research, review)

use chrono::Utc;
use serde::{Deserialize, Serialize};
use smooth_policy::{
    AccessRequestConfig, AuthConfig, BeadsPolicy, FilesystemPolicy, LeaderNetworkConfig, McpPolicy, NetworkPolicy, NetworkRule, Policy, PolicyMetadata,
    ToolsPolicy,
};

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

/// The kind of task an operator is performing.
///
/// Different task types get different tool sets, filesystem permissions,
/// network access, and auto-approve rules — even within the same phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskType {
    /// Write code: full tool access, writable filesystem in execute/finalize.
    Coding,
    /// Gather information: read-only filesystem, no write/edit tools.
    Research,
    /// Adversarial review: read-only, minimal network, no auto-approve.
    Review,
}

/// Generate a task-type-specific policy for an operator.
///
/// This is a higher-level alternative to [`generate_policy`] that tailors
/// tools, network, filesystem, and auto-approve rules to the given
/// [`TaskType`].
///
/// # Errors
/// Returns error if the policy cannot be serialized.
pub fn generate_policy_for_task(
    operator_id: &str,
    bead_id: &str,
    phase: &str,
    token: &str,
    bead_deps: &[String],
    task_type: TaskType,
) -> anyhow::Result<String> {
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
        network: task_network_policy(phase, task_type),
        beads: beads_policy(bead_id, bead_deps, phase),
        filesystem: task_filesystem_policy(phase, task_type),
        tools: task_tools_policy(phase, task_type),
        mcp: McpPolicy {
            allow_servers: vec!["smooth-tools".into()],
            deny_unknown_servers: true,
            allow_server_install: false,
        },
        access_requests: task_access_requests(task_type),
    };

    Ok(policy.to_toml()?)
}

// ---------------------------------------------------------------------------
// Task-type-specific helpers
// ---------------------------------------------------------------------------

fn task_tools_policy(phase: &str, task_type: TaskType) -> ToolsPolicy {
    let beads_tools: Vec<String> = vec!["beads_context".into(), "beads_message".into(), "progress".into()];

    let allow = match task_type {
        TaskType::Coding => {
            let mut tools = vec![
                "read_file".into(),
                "write_file".into(),
                "edit_file".into(),
                "bash".into(),
                "code_search".into(),
                "find_definition".into(),
                "repo_map".into(),
            ];
            tools.extend(beads_tools);
            if matches!(phase, "execute" | "finalize") {
                tools.extend([
                    "artifact_write".into(),
                    "lint_fix".into(),
                    "test_run".into(),
                    "spawn_subtask".into(),
                    "git_commit".into(),
                ]);
            }
            tools
        }
        TaskType::Research => {
            let mut tools = vec![
                "read_file".into(),
                "bash".into(),
                "code_search".into(),
                "find_definition".into(),
                "repo_map".into(),
            ];
            tools.extend(beads_tools);
            tools
        }
        TaskType::Review => {
            let mut tools = vec![
                "read_file".into(),
                "bash".into(),
                "code_search".into(),
                "find_definition".into(),
                "repo_map".into(),
                "git_diff".into(),
            ];
            tools.extend(beads_tools);
            tools
        }
    };

    ToolsPolicy {
        allow,
        deny: vec!["workflow".into()],
    }
}

fn task_filesystem_policy(phase: &str, task_type: TaskType) -> FilesystemPolicy {
    let writable = match task_type {
        TaskType::Coding => smooth_policy::phase_filesystem_writable(phase),
        TaskType::Research | TaskType::Review => false,
    };

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
        writable,
    }
}

fn task_network_policy(phase: &str, task_type: TaskType) -> NetworkPolicy {
    let allow = match task_type {
        TaskType::Coding => {
            let mut rules = vec![
                NetworkRule {
                    domain: "opencode.ai".into(),
                    path: None,
                    methods: None,
                },
                NetworkRule {
                    domain: "registry.npmjs.org".into(),
                    path: None,
                    methods: None,
                },
                NetworkRule {
                    domain: "pypi.org".into(),
                    path: None,
                    methods: None,
                },
                NetworkRule {
                    domain: "crates.io".into(),
                    path: None,
                    methods: None,
                },
                NetworkRule {
                    domain: "docs.rs".into(),
                    path: None,
                    methods: None,
                },
                NetworkRule {
                    domain: "developer.mozilla.org".into(),
                    path: None,
                    methods: None,
                },
            ];
            if matches!(phase, "execute" | "finalize") {
                rules.push(NetworkRule {
                    domain: "api.github.com".into(),
                    path: Some("/repos/SmooAI/*".into()),
                    methods: None,
                });
            }
            rules
        }
        TaskType::Research => {
            vec![
                NetworkRule {
                    domain: "opencode.ai".into(),
                    path: None,
                    methods: None,
                },
                NetworkRule {
                    domain: "stackoverflow.com".into(),
                    path: None,
                    methods: None,
                },
                NetworkRule {
                    domain: "github.com".into(),
                    path: None,
                    methods: None,
                },
                NetworkRule {
                    domain: "docs.rs".into(),
                    path: None,
                    methods: None,
                },
                NetworkRule {
                    domain: "developer.mozilla.org".into(),
                    path: None,
                    methods: None,
                },
                NetworkRule {
                    domain: "registry.npmjs.org".into(),
                    path: None,
                    methods: None,
                },
                NetworkRule {
                    domain: "pypi.org".into(),
                    path: None,
                    methods: None,
                },
                NetworkRule {
                    domain: "crates.io".into(),
                    path: None,
                    methods: None,
                },
            ]
        }
        TaskType::Review => {
            vec![NetworkRule {
                domain: "opencode.ai".into(),
                path: None,
                methods: None,
            }]
        }
    };

    NetworkPolicy {
        allow,
        max_response_bytes: 52_428_800,
        leader: LeaderNetworkConfig::default(),
    }
}

fn task_access_requests(task_type: TaskType) -> AccessRequestConfig {
    match task_type {
        TaskType::Coding => AccessRequestConfig {
            enabled: true,
            auto_approve_domains: vec![
                "*.npmjs.org".into(),
                "*.pypi.org".into(),
                "*.crates.io".into(),
                "docs.rs".into(),
                "developer.mozilla.org".into(),
            ],
            auto_approve_tools: vec!["lint_fix".into(), "test_run".into(), "read_file".into(), "code_search".into()],
        },
        TaskType::Research => AccessRequestConfig {
            enabled: true,
            auto_approve_domains: vec![
                "*.npmjs.org".into(),
                "*.pypi.org".into(),
                "*.crates.io".into(),
                "docs.rs".into(),
                "developer.mozilla.org".into(),
                "stackoverflow.com".into(),
                "github.com".into(),
            ],
            auto_approve_tools: vec!["read_file".into(), "code_search".into(), "find_definition".into(), "repo_map".into()],
        },
        TaskType::Review => AccessRequestConfig {
            enabled: true,
            auto_approve_domains: vec![],
            auto_approve_tools: vec![],
        },
    }
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

    // -----------------------------------------------------------------------
    // Task-type policy tests
    // -----------------------------------------------------------------------

    #[test]
    fn coding_task_execute_has_write_tools_and_writable_fs() {
        let toml = generate_policy_for_task("op-c", "bead-1", "execute", "tok", &[], TaskType::Coding).expect("generate");
        let policy = smooth_policy::Policy::from_toml(&toml).expect("parse");

        assert!(policy.filesystem.writable, "coding execute should be writable");
        assert!(policy.tools.can_use("write_file"), "coding should have write_file");
        assert!(policy.tools.can_use("edit_file"), "coding should have edit_file");
        assert!(policy.tools.can_use("bash"), "coding should have bash");
        assert!(policy.tools.can_use("read_file"), "coding should have read_file");
        assert!(policy.tools.can_use("git_commit"), "coding execute should have git_commit");
        assert!(policy.network.is_allowed("docs.rs", "/"), "coding should reach docs.rs");
        assert!(policy.network.is_allowed("developer.mozilla.org", "/"), "coding should reach MDN");
    }

    #[test]
    fn coding_task_assess_is_read_only() {
        let toml = generate_policy_for_task("op-c2", "bead-1", "assess", "tok", &[], TaskType::Coding).expect("generate");
        let policy = smooth_policy::Policy::from_toml(&toml).expect("parse");

        assert!(!policy.filesystem.writable, "coding assess should be read-only");
        // Still has write tools in the allow list (tool access), but FS is read-only
        assert!(policy.tools.can_use("write_file"), "tool is allowed even in assess");
        assert!(!policy.tools.can_use("git_commit"), "no git_commit outside execute/finalize");
    }

    #[test]
    fn research_task_always_read_only_no_write_tools() {
        for phase in &["assess", "execute", "finalize", "review"] {
            let toml = generate_policy_for_task("op-r", "bead-1", phase, "tok", &[], TaskType::Research).expect("generate");
            let policy = smooth_policy::Policy::from_toml(&toml).expect("parse");

            assert!(!policy.filesystem.writable, "research should always be read-only in {phase}");
            assert!(!policy.tools.can_use("write_file"), "research should not have write_file in {phase}");
            assert!(!policy.tools.can_use("edit_file"), "research should not have edit_file in {phase}");
            assert!(!policy.tools.can_use("git_commit"), "research should not have git_commit in {phase}");
            assert!(policy.tools.can_use("read_file"), "research should have read_file in {phase}");
            assert!(policy.tools.can_use("code_search"), "research should have code_search in {phase}");
        }
    }

    #[test]
    fn review_task_minimal_network_no_writes() {
        let toml = generate_policy_for_task("op-v", "bead-1", "execute", "tok", &[], TaskType::Review).expect("generate");
        let policy = smooth_policy::Policy::from_toml(&toml).expect("parse");

        assert!(!policy.filesystem.writable, "review should be read-only");
        assert!(!policy.tools.can_use("write_file"), "review should not write");
        assert!(!policy.tools.can_use("edit_file"), "review should not edit");
        assert!(policy.tools.can_use("git_diff"), "review should have git_diff");

        // Minimal network: only LLM API
        assert!(policy.network.is_allowed("opencode.ai", "/"), "review needs LLM");
        assert!(
            !policy.network.is_allowed("api.github.com", "/repos/SmooAI/smooth"),
            "review should not reach GitHub"
        );
        assert!(!policy.network.is_allowed("stackoverflow.com", "/"), "review should not reach SO");

        // No auto-approve
        assert!(policy.access_requests.auto_approve_domains.is_empty(), "review has no auto-approve domains");
        assert!(policy.access_requests.auto_approve_tools.is_empty(), "review has no auto-approve tools");
    }

    #[test]
    fn all_task_types_include_beads_tools() {
        for task_type in [TaskType::Coding, TaskType::Research, TaskType::Review] {
            let toml = generate_policy_for_task("op-b", "bead-1", "execute", "tok", &[], task_type).expect("generate");
            let policy = smooth_policy::Policy::from_toml(&toml).expect("parse");

            assert!(policy.tools.can_use("beads_context"), "{task_type:?} should have beads_context");
            assert!(policy.tools.can_use("beads_message"), "{task_type:?} should have beads_message");
            assert!(policy.tools.can_use("progress"), "{task_type:?} should have progress");
        }
    }

    #[test]
    fn task_type_serialization_roundtrip() {
        for task_type in [TaskType::Coding, TaskType::Research, TaskType::Review] {
            let json = serde_json::to_string(&task_type).expect("serialize");
            let back: TaskType = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(back, task_type, "roundtrip failed for {task_type:?}");
        }
        // Also verify the snake_case rename
        assert_eq!(serde_json::to_string(&TaskType::Coding).expect("ser"), "\"coding\"");
        assert_eq!(serde_json::to_string(&TaskType::Research).expect("ser"), "\"research\"");
        assert_eq!(serde_json::to_string(&TaskType::Review).expect("ser"), "\"review\"");
    }
}

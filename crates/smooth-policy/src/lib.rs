use std::path::Path;

use chrono::{DateTime, Utc};
use globset::{Glob, GlobSet, GlobSetBuilder};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Error
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum PolicyError {
    #[error("failed to parse policy TOML: {0}")]
    Parse(#[from] toml::de::Error),

    #[error("failed to serialize policy TOML: {0}")]
    Serialize(#[from] toml::ser::Error),

    #[error("invalid glob pattern '{pattern}': {source}")]
    Glob { pattern: String, source: globset::Error },

    #[error("policy validation failed: {0}")]
    Validation(String),
}

pub type Result<T> = std::result::Result<T, PolicyError>;

// ---------------------------------------------------------------------------
// Top-level Policy
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Policy {
    pub metadata: PolicyMetadata,
    pub auth: AuthConfig,
    pub network: NetworkPolicy,
    #[serde(default)]
    pub beads: BeadsPolicy,
    #[serde(default)]
    pub filesystem: FilesystemPolicy,
    #[serde(default)]
    pub tools: ToolsPolicy,
    #[serde(default)]
    pub mcp: McpPolicy,
    #[serde(default)]
    pub access_requests: AccessRequestConfig,
}

impl Policy {
    /// # Errors
    /// Returns `PolicyError::Parse` if the TOML is invalid, or `PolicyError::Validation`
    /// if required fields are missing.
    pub fn from_toml(s: &str) -> Result<Self> {
        let policy: Self = toml::from_str(s)?;
        policy.validate()?;
        Ok(policy)
    }

    /// # Errors
    /// Returns `PolicyError::Serialize` if serialization fails.
    pub fn to_toml(&self) -> Result<String> {
        Ok(toml::to_string_pretty(self)?)
    }

    fn validate(&self) -> Result<()> {
        if self.metadata.operator_id.is_empty() {
            return Err(PolicyError::Validation("operator_id is required".into()));
        }
        if self.auth.token.is_empty() {
            return Err(PolicyError::Validation("auth.token is required".into()));
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Metadata
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolicyMetadata {
    pub operator_id: String,
    #[serde(default)]
    pub bead_id: String,
    #[serde(default)]
    pub phase: String,
    #[serde(default)]
    pub generated_at: Option<DateTime<Utc>>,
}

// ---------------------------------------------------------------------------
// Auth
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthConfig {
    pub token: String,
    #[serde(default = "default_leader_url")]
    pub leader_url: String,
}

fn default_leader_url() -> String {
    "http://host.containers.internal:4400".into()
}

// ---------------------------------------------------------------------------
// Network
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkPolicy {
    #[serde(default)]
    pub allow: Vec<NetworkRule>,
    #[serde(default = "default_max_response_bytes")]
    pub max_response_bytes: u64,
    #[serde(default)]
    pub leader: LeaderNetworkConfig,
}

const fn default_max_response_bytes() -> u64 {
    52_428_800 // 50 MB
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkRule {
    pub domain: String,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub methods: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LeaderNetworkConfig {
    #[serde(default = "default_true")]
    pub always_allowed: bool,
}

impl Default for LeaderNetworkConfig {
    fn default() -> Self {
        Self { always_allowed: true }
    }
}

impl NetworkPolicy {
    /// Check whether a request to `domain` at `path` is allowed.
    pub fn is_allowed(&self, domain: &str, path: &str) -> bool {
        for rule in &self.allow {
            if domain_matches(&rule.domain, domain) && path_matches(rule.path.as_deref(), path) {
                return true;
            }
        }
        false
    }
}

fn domain_matches(pattern: &str, domain: &str) -> bool {
    if pattern.starts_with("*.") {
        let suffix = &pattern[1..]; // e.g. ".npmjs.org"
        domain.ends_with(suffix) || domain == &pattern[2..]
    } else {
        pattern == domain
    }
}

fn path_matches(pattern: Option<&str>, path: &str) -> bool {
    pattern.is_none_or(|pat| pat.strip_suffix('*').map_or(pat == path, |prefix| path.starts_with(prefix)))
}

// ---------------------------------------------------------------------------
// Beads
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BeadsPolicy {
    #[serde(default)]
    pub accessible: Vec<String>,
    #[serde(default)]
    pub include_dependencies: bool,
    #[serde(default = "default_max_depth")]
    pub max_depth: u32,
}

const fn default_max_depth() -> u32 {
    2
}

impl BeadsPolicy {
    pub fn can_access(&self, bead_id: &str) -> bool {
        self.accessible.iter().any(|id| id == bead_id)
    }
}

// ---------------------------------------------------------------------------
// Filesystem
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FilesystemPolicy {
    #[serde(default)]
    pub deny_patterns: Vec<String>,
    #[serde(default = "default_true")]
    pub writable: bool,
}

impl Default for FilesystemPolicy {
    fn default() -> Self {
        Self {
            deny_patterns: vec![],
            writable: true,
        }
    }
}

impl FilesystemPolicy {
    /// Build a `GlobSet` from deny patterns for efficient matching.
    ///
    /// # Errors
    /// Returns `PolicyError::Glob` if any deny pattern is an invalid glob.
    pub fn deny_globset(&self) -> Result<GlobSet> {
        let mut builder = GlobSetBuilder::new();
        for pat in &self.deny_patterns {
            let glob = Glob::new(pat).map_err(|e| PolicyError::Glob {
                pattern: pat.clone(),
                source: e,
            })?;
            builder.add(glob);
        }
        builder.build().map_err(|e| PolicyError::Glob {
            pattern: "<combined>".into(),
            source: e,
        })
    }

    /// Check if a file path should be denied.
    ///
    /// # Errors
    /// Returns `PolicyError::Glob` if any deny pattern is an invalid glob.
    pub fn is_denied(&self, path: &str) -> Result<bool> {
        let globset = self.deny_globset()?;
        Ok(globset.is_match(Path::new(path)))
    }
}

// ---------------------------------------------------------------------------
// Tools
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ToolsPolicy {
    #[serde(default)]
    pub allow: Vec<String>,
    #[serde(default)]
    pub deny: Vec<String>,
}

impl ToolsPolicy {
    /// A tool is usable if it is in the allow list and NOT in the deny list.
    pub fn can_use(&self, tool_name: &str) -> bool {
        if self.deny.iter().any(|d| d == tool_name) {
            return false;
        }
        // Empty allow list = nothing allowed (default deny)
        self.allow.iter().any(|a| a == tool_name)
    }
}

// ---------------------------------------------------------------------------
// MCP
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpPolicy {
    #[serde(default)]
    pub allow_servers: Vec<String>,
    #[serde(default = "default_true")]
    pub deny_unknown_servers: bool,
    #[serde(default)]
    pub allow_server_install: bool,
}

impl Default for McpPolicy {
    fn default() -> Self {
        Self {
            allow_servers: vec![],
            deny_unknown_servers: true,
            allow_server_install: false,
        }
    }
}

impl McpPolicy {
    pub fn can_connect(&self, server_name: &str) -> bool {
        if self.allow_servers.iter().any(|s| s == server_name) {
            return true;
        }
        !self.deny_unknown_servers
    }
}

// ---------------------------------------------------------------------------
// Access Requests
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccessRequestConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub auto_approve_domains: Vec<String>,
    #[serde(default)]
    pub auto_approve_tools: Vec<String>,
}

impl Default for AccessRequestConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            auto_approve_domains: vec![],
            auto_approve_tools: vec![],
        }
    }
}

impl AccessRequestConfig {
    pub fn should_auto_approve_domain(&self, domain: &str) -> bool {
        self.auto_approve_domains.iter().any(|pat| domain_matches(pat, domain))
    }

    pub fn should_auto_approve_tool(&self, tool_name: &str) -> bool {
        self.auto_approve_tools.iter().any(|t| t == tool_name)
    }
}

// ---------------------------------------------------------------------------
// Enterprise Policy — permanent team-maintained firewall rules
// ---------------------------------------------------------------------------

/// Enterprise policy: permanent deny rules that cannot be overridden.
///
/// Loaded from `SMOOTH_ENTERPRISE_POLICY` env var or `.smooth/enterprise-policy.toml`.
/// These rules are merged into every task policy and cannot be removed by
/// agents or per-task settings.
///
/// ```toml
/// [network]
/// deny_domains = ["*.prod.internal", "prod-db.company.com"]
///
/// [filesystem]
/// deny_patterns = ["/etc/passwd", ".env.production"]
///
/// [tools]
/// deny = ["rm_rf", "drop_database"]
///
/// [mcp]
/// deny_servers = ["untrusted-server"]
/// ```
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EnterprisePolicy {
    #[serde(default)]
    pub network: EnterpriseNetworkPolicy,
    #[serde(default)]
    pub filesystem: EnterpriseFilesystemPolicy,
    #[serde(default)]
    pub tools: EnterpriseToolsPolicy,
    #[serde(default)]
    pub mcp: EnterpriseMcpPolicy,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EnterpriseNetworkPolicy {
    /// Domains that are permanently blocked. Supports wildcards (e.g. `*.prod.internal`).
    #[serde(default)]
    pub deny_domains: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EnterpriseFilesystemPolicy {
    /// Glob patterns for paths that are permanently denied.
    #[serde(default)]
    pub deny_patterns: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EnterpriseToolsPolicy {
    /// Tools that are permanently denied — no task can allow them.
    #[serde(default)]
    pub deny: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EnterpriseMcpPolicy {
    /// MCP servers that are permanently blocked.
    #[serde(default)]
    pub deny_servers: Vec<String>,
}

impl EnterprisePolicy {
    /// Parse an enterprise policy from TOML.
    ///
    /// # Errors
    /// Returns `PolicyError::Parse` if the TOML is invalid.
    pub fn from_toml(s: &str) -> Result<Self> {
        Ok(toml::from_str(s)?)
    }

    /// Serialize to TOML.
    ///
    /// # Errors
    /// Returns `PolicyError::Serialize` if serialization fails.
    pub fn to_toml(&self) -> Result<String> {
        Ok(toml::to_string_pretty(self)?)
    }

    /// Load from file path, if it exists.
    pub fn load_from_file(path: &Path) -> Option<Self> {
        let content = std::fs::read_to_string(path).ok()?;
        Self::from_toml(&content).ok()
    }

    /// Load from the default location: `SMOOTH_ENTERPRISE_POLICY` env var,
    /// or `.smooth/enterprise-policy.toml` in the current directory.
    pub fn load_default() -> Option<Self> {
        // Check env var first
        if let Ok(path) = std::env::var("SMOOTH_ENTERPRISE_POLICY") {
            if let Some(policy) = Self::load_from_file(Path::new(&path)) {
                return Some(policy);
            }
        }
        // Fall back to .smooth/enterprise-policy.toml
        let cwd = std::env::current_dir().ok()?;
        Self::load_from_file(&cwd.join(".smooth").join("enterprise-policy.toml"))
    }

    /// Returns true if this policy has no rules.
    pub fn is_empty(&self) -> bool {
        self.network.deny_domains.is_empty() && self.filesystem.deny_patterns.is_empty() && self.tools.deny.is_empty() && self.mcp.deny_servers.is_empty()
    }
}

impl Policy {
    /// Merge enterprise policy into this task policy. Enterprise rules are
    /// permanent and cannot be overridden:
    ///
    /// - **Network**: denied domains are removed from the allow list
    /// - **Filesystem**: enterprise deny patterns are added to the deny list
    /// - **Tools**: enterprise denied tools are added to the deny list and
    ///   removed from the allow list
    /// - **MCP**: enterprise denied servers are removed from allow_servers
    pub fn merge_enterprise(&mut self, enterprise: &EnterprisePolicy) {
        // Network: remove denied domains from allow list
        if !enterprise.network.deny_domains.is_empty() {
            self.network.allow.retain(|rule| {
                !enterprise
                    .network
                    .deny_domains
                    .iter()
                    .any(|denied| domain_matches(denied, &rule.domain) || domain_matches(&rule.domain, denied))
            });
        }

        // Filesystem: add enterprise deny patterns (dedup)
        for pattern in &enterprise.filesystem.deny_patterns {
            if !self.filesystem.deny_patterns.contains(pattern) {
                self.filesystem.deny_patterns.push(pattern.clone());
            }
        }

        // Tools: add enterprise denies, remove from allows
        for tool in &enterprise.tools.deny {
            if !self.tools.deny.contains(tool) {
                self.tools.deny.push(tool.clone());
            }
            self.tools.allow.retain(|a| a != tool);
        }

        // MCP: remove denied servers from allow list
        if !enterprise.mcp.deny_servers.is_empty() {
            self.mcp.allow_servers.retain(|s| !enterprise.mcp.deny_servers.contains(s));
        }
    }
}

// ---------------------------------------------------------------------------
// Phase defaults — generate policies per orchestration phase
// ---------------------------------------------------------------------------

pub fn phase_network_defaults(phase: &str) -> Vec<NetworkRule> {
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
    ];

    match phase {
        "orchestrate" | "execute" | "finalize" => {
            rules.push(NetworkRule {
                domain: "api.github.com".into(),
                path: Some("/repos/SmooAI/*".into()),
                methods: None,
            });
        }
        _ => {}
    }

    rules
}

pub fn phase_filesystem_writable(phase: &str) -> bool {
    matches!(phase, "execute" | "finalize")
}

pub fn phase_beads_depth(phase: &str) -> u32 {
    match phase {
        "assess" => 1,
        _ => 2,
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

const fn default_true() -> bool {
    true
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const EXAMPLE_POLICY: &str = r#"
[metadata]
operator_id = "operator-a3f8c2d1"
bead_id = "smooth-abc123"
phase = "execute"
generated_at = "2026-04-03T19:00:00Z"

[auth]
token = "smth_op_a3f8c2d1_7kJ9xM2"
leader_url = "http://host.containers.internal:4400"

[network]
max_response_bytes = 52428800

[[network.allow]]
domain = "opencode.ai"

[[network.allow]]
domain = "registry.npmjs.org"

[[network.allow]]
domain = "api.github.com"
path = "/repos/SmooAI/*"

[network.leader]
always_allowed = true

[beads]
accessible = ["smooth-abc123"]
include_dependencies = true
max_depth = 2

[filesystem]
deny_patterns = ["*.env", "*.pem", "*.key", ".ssh/*", ".aws/*"]
writable = true

[tools]
allow = ["beads_context", "beads_message", "progress", "code_search"]
deny = ["workflow"]

[mcp]
allow_servers = ["smooth-tools"]
deny_unknown_servers = true
allow_server_install = false

[access_requests]
enabled = true
auto_approve_domains = ["*.npmjs.org", "*.pypi.org"]
auto_approve_tools = ["lint_fix", "test_run"]
"#;

    #[test]
    fn parse_full_policy() {
        let policy = Policy::from_toml(EXAMPLE_POLICY).expect("should parse");
        assert_eq!(policy.metadata.operator_id, "operator-a3f8c2d1");
        assert_eq!(policy.metadata.bead_id, "smooth-abc123");
        assert_eq!(policy.metadata.phase, "execute");
        assert_eq!(policy.auth.token, "smth_op_a3f8c2d1_7kJ9xM2");
        assert_eq!(policy.network.allow.len(), 3);
        assert_eq!(policy.filesystem.deny_patterns.len(), 5);
        assert!(policy.filesystem.writable);
    }

    #[test]
    fn roundtrip_toml() {
        let policy = Policy::from_toml(EXAMPLE_POLICY).expect("should parse");
        let serialized = policy.to_toml().expect("should serialize");
        let reparsed = Policy::from_toml(&serialized).expect("should reparse");
        assert_eq!(reparsed.metadata.operator_id, policy.metadata.operator_id);
        assert_eq!(reparsed.network.allow.len(), policy.network.allow.len());
    }

    #[test]
    fn network_domain_matching() {
        let policy = Policy::from_toml(EXAMPLE_POLICY).expect("should parse");
        assert!(policy.network.is_allowed("opencode.ai", "/zen/v1/chat"));
        assert!(policy.network.is_allowed("registry.npmjs.org", "/express"));
        assert!(policy.network.is_allowed("api.github.com", "/repos/SmooAI/smooth"));
        assert!(!policy.network.is_allowed("api.github.com", "/users/someone"));
        assert!(!policy.network.is_allowed("evil.com", "/"));
    }

    #[test]
    fn wildcard_domain_matching() {
        assert!(domain_matches("*.npmjs.org", "registry.npmjs.org"));
        assert!(domain_matches("*.npmjs.org", "npmjs.org"));
        assert!(!domain_matches("*.npmjs.org", "evil.org"));
        assert!(domain_matches("opencode.ai", "opencode.ai"));
        assert!(!domain_matches("opencode.ai", "notopencode.ai"));
    }

    #[test]
    fn path_matching_wildcard() {
        assert!(path_matches(Some("/repos/SmooAI/*"), "/repos/SmooAI/smooth"));
        assert!(path_matches(Some("/repos/SmooAI/*"), "/repos/SmooAI/beads"));
        assert!(!path_matches(Some("/repos/SmooAI/*"), "/repos/OtherOrg/repo"));
        assert!(path_matches(None, "/anything/goes"));
    }

    #[test]
    fn beads_access() {
        let policy = Policy::from_toml(EXAMPLE_POLICY).expect("should parse");
        assert!(policy.beads.can_access("smooth-abc123"));
        assert!(!policy.beads.can_access("smooth-other"));
    }

    #[test]
    fn filesystem_deny() {
        let policy = Policy::from_toml(EXAMPLE_POLICY).expect("should parse");
        assert!(policy.filesystem.is_denied(".env").expect("glob"));
        assert!(policy.filesystem.is_denied("secrets.pem").expect("glob"));
        assert!(policy.filesystem.is_denied("my.key").expect("glob"));
        assert!(policy.filesystem.is_denied(".ssh/id_rsa").expect("glob"));
        assert!(policy.filesystem.is_denied(".aws/credentials").expect("glob"));
        assert!(!policy.filesystem.is_denied("src/main.rs").expect("glob"));
        assert!(!policy.filesystem.is_denied("Cargo.toml").expect("glob"));
    }

    #[test]
    fn tools_allow_deny() {
        let policy = Policy::from_toml(EXAMPLE_POLICY).expect("should parse");
        assert!(policy.tools.can_use("beads_context"));
        assert!(policy.tools.can_use("progress"));
        assert!(!policy.tools.can_use("workflow")); // explicitly denied
        assert!(!policy.tools.can_use("unknown_tool")); // not in allow list
    }

    #[test]
    fn mcp_server_access() {
        let policy = Policy::from_toml(EXAMPLE_POLICY).expect("should parse");
        assert!(policy.mcp.can_connect("smooth-tools"));
        assert!(!policy.mcp.can_connect("unknown-server")); // deny_unknown_servers = true
    }

    #[test]
    fn access_request_auto_approve() {
        let policy = Policy::from_toml(EXAMPLE_POLICY).expect("should parse");
        assert!(policy.access_requests.should_auto_approve_domain("registry.npmjs.org"));
        assert!(policy.access_requests.should_auto_approve_domain("pypi.org"));
        assert!(!policy.access_requests.should_auto_approve_domain("api.stripe.com"));
        assert!(policy.access_requests.should_auto_approve_tool("lint_fix"));
        assert!(!policy.access_requests.should_auto_approve_tool("workflow"));
    }

    #[test]
    fn phase_defaults() {
        let assess_rules = phase_network_defaults("assess");
        assert_eq!(assess_rules.len(), 4); // LLM + 3 registries, no GitHub
        let execute_rules = phase_network_defaults("execute");
        assert_eq!(execute_rules.len(), 5); // + GitHub

        assert!(!phase_filesystem_writable("assess"));
        assert!(!phase_filesystem_writable("plan"));
        assert!(phase_filesystem_writable("execute"));
        assert!(phase_filesystem_writable("finalize"));
        assert!(!phase_filesystem_writable("review"));

        assert_eq!(phase_beads_depth("assess"), 1);
        assert_eq!(phase_beads_depth("execute"), 2);
    }

    #[test]
    fn validation_rejects_empty_operator_id() {
        let bad = EXAMPLE_POLICY.replace("operator-a3f8c2d1", "");
        let result = Policy::from_toml(&bad);
        assert!(result.is_err());
    }

    #[test]
    fn validation_rejects_empty_token() {
        let bad = EXAMPLE_POLICY.replace("smth_op_a3f8c2d1_7kJ9xM2", "");
        let result = Policy::from_toml(&bad);
        assert!(result.is_err());
    }

    #[test]
    fn default_deny_empty_allow() {
        let tools = ToolsPolicy { allow: vec![], deny: vec![] };
        assert!(!tools.can_use("anything"));
    }

    #[test]
    fn mcp_allow_unknown_when_not_denied() {
        let mcp = McpPolicy {
            allow_servers: vec!["smooth-tools".into()],
            deny_unknown_servers: false,
            allow_server_install: false,
        };
        assert!(mcp.can_connect("smooth-tools"));
        assert!(mcp.can_connect("any-other-server")); // deny_unknown_servers = false
    }

    #[test]
    fn filesystem_no_deny_patterns() {
        let fs = FilesystemPolicy {
            deny_patterns: vec![],
            writable: true,
        };
        assert!(!fs.is_denied("anything.txt").expect("glob"));
    }

    // ── Enterprise policy tests ──────────────────────────────────────

    const ENTERPRISE_TOML: &str = r#"
[network]
deny_domains = ["*.prod.internal", "prod-db.company.com"]

[filesystem]
deny_patterns = ["/etc/passwd", "*.production.env"]

[tools]
deny = ["rm_rf", "drop_database", "workflow"]

[mcp]
deny_servers = ["untrusted-server"]
"#;

    #[test]
    fn parse_enterprise_policy() {
        let ep = EnterprisePolicy::from_toml(ENTERPRISE_TOML).expect("parse");
        assert_eq!(ep.network.deny_domains.len(), 2);
        assert_eq!(ep.filesystem.deny_patterns.len(), 2);
        assert_eq!(ep.tools.deny.len(), 3);
        assert_eq!(ep.mcp.deny_servers.len(), 1);
    }

    #[test]
    fn enterprise_roundtrip() {
        let ep = EnterprisePolicy::from_toml(ENTERPRISE_TOML).expect("parse");
        let toml = ep.to_toml().expect("serialize");
        let reparsed = EnterprisePolicy::from_toml(&toml).expect("reparse");
        assert_eq!(reparsed.network.deny_domains.len(), 2);
    }

    #[test]
    fn enterprise_empty() {
        let ep = EnterprisePolicy::default();
        assert!(ep.is_empty());
        let ep = EnterprisePolicy::from_toml(ENTERPRISE_TOML).expect("parse");
        assert!(!ep.is_empty());
    }

    #[test]
    fn merge_removes_denied_network_domains() {
        let mut policy = Policy::from_toml(EXAMPLE_POLICY).expect("parse");
        let orig_count = policy.network.allow.len();
        assert!(orig_count > 0);

        // Enterprise blocks opencode.ai
        let ep = EnterprisePolicy {
            network: EnterpriseNetworkPolicy {
                deny_domains: vec!["opencode.ai".to_string()],
            },
            ..Default::default()
        };
        policy.merge_enterprise(&ep);

        // opencode.ai should be removed
        assert_eq!(policy.network.allow.len(), orig_count - 1);
        assert!(!policy.network.is_allowed("opencode.ai", "/anything"));
        // Other domains still allowed
        assert!(policy.network.is_allowed("registry.npmjs.org", "/express"));
    }

    #[test]
    fn merge_removes_wildcard_denied_domains() {
        let mut policy = Policy::from_toml(EXAMPLE_POLICY).expect("parse");

        let ep = EnterprisePolicy {
            network: EnterpriseNetworkPolicy {
                deny_domains: vec!["*.github.com".to_string()],
            },
            ..Default::default()
        };
        policy.merge_enterprise(&ep);

        assert!(!policy.network.is_allowed("api.github.com", "/repos/SmooAI/smooth"));
    }

    #[test]
    fn merge_adds_filesystem_deny_patterns() {
        let mut policy = Policy::from_toml(EXAMPLE_POLICY).expect("parse");
        let orig_count = policy.filesystem.deny_patterns.len();

        let ep = EnterprisePolicy {
            filesystem: EnterpriseFilesystemPolicy {
                deny_patterns: vec!["/etc/passwd".to_string(), "*.env".to_string()], // *.env already exists
            },
            ..Default::default()
        };
        policy.merge_enterprise(&ep);

        // /etc/passwd added, *.env deduped
        assert_eq!(policy.filesystem.deny_patterns.len(), orig_count + 1);
        assert!(policy.filesystem.deny_patterns.contains(&"/etc/passwd".to_string()));
    }

    #[test]
    fn merge_denies_tools_and_removes_from_allow() {
        let mut policy = Policy::from_toml(EXAMPLE_POLICY).expect("parse");
        assert!(policy.tools.can_use("beads_context")); // allowed

        let ep = EnterprisePolicy {
            tools: EnterpriseToolsPolicy {
                deny: vec!["beads_context".to_string(), "evil_tool".to_string()],
            },
            ..Default::default()
        };
        policy.merge_enterprise(&ep);

        // beads_context removed from allow AND added to deny
        assert!(!policy.tools.can_use("beads_context"));
        assert!(policy.tools.deny.contains(&"beads_context".to_string()));
        assert!(policy.tools.deny.contains(&"evil_tool".to_string()));
        assert!(!policy.tools.allow.contains(&"beads_context".to_string()));
    }

    #[test]
    fn merge_removes_denied_mcp_servers() {
        let mut policy = Policy::from_toml(EXAMPLE_POLICY).expect("parse");
        assert!(policy.mcp.can_connect("smooth-tools"));

        let ep = EnterprisePolicy {
            mcp: EnterpriseMcpPolicy {
                deny_servers: vec!["smooth-tools".to_string()],
            },
            ..Default::default()
        };
        policy.merge_enterprise(&ep);

        assert!(!policy.mcp.can_connect("smooth-tools"));
    }

    #[test]
    fn merge_empty_enterprise_is_noop() {
        let mut policy = Policy::from_toml(EXAMPLE_POLICY).expect("parse");
        let before = policy.to_toml().expect("serialize");

        policy.merge_enterprise(&EnterprisePolicy::default());

        let after = policy.to_toml().expect("serialize");
        assert_eq!(before, after);
    }

    #[test]
    fn merge_full_enterprise_policy() {
        let mut policy = Policy::from_toml(EXAMPLE_POLICY).expect("parse");
        let ep = EnterprisePolicy::from_toml(ENTERPRISE_TOML).expect("parse");

        policy.merge_enterprise(&ep);

        // workflow was already in deny, should not duplicate
        assert_eq!(policy.tools.deny.iter().filter(|t| *t == "workflow").count(), 1);
        // rm_rf and drop_database added
        assert!(policy.tools.deny.contains(&"rm_rf".to_string()));
        // prod domains blocked
        assert!(!policy.network.is_allowed("api.prod.internal", "/"));
    }
}

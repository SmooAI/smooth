//! Production wiring: `AppState` implements `smooth_wonk::grpc::Checker`.
//!
//! Pearl th-893801 iter-3b. Lets the existing Wonk AppState serve the
//! gRPC Checker trait — same decision logic the HTTP `/check/*`
//! handlers run, but exposed over UDS gRPC for callers inside the
//! single sandbox VM (operator-runner's WonkHook in iter-3f).
//!
//! ## Duplication note
//!
//! The decision logic here duplicates the HTTP handlers in
//! `server.rs`. Intentionally so for iter-3b — splitting into shared
//! methods on AppState would touch too many call sites at once. The
//! cleanup pass (Phase 4 / iter-3f when the SMOOTH_SINGLE_PROCESS
//! flag becomes the default) folds the two paths into one.

use crate::grpc::{Checker, CliReq, FileAccess, FileReq, NetworkReq, PolicySummary, ReloadResult, ToolReq, Verdict};
use crate::server::AppState;
use async_trait::async_trait;
use smooth_narc::judge::{Decision, JudgeKind, JudgeRequest};

#[async_trait]
impl Checker for AppState {
    async fn check_network(&self, req: NetworkReq) -> Verdict {
        let policy = self.policy().load();

        // 1. Static policy allowlist.
        if policy.network.is_allowed(&req.domain, &req.path) {
            return Verdict {
                allowed: true,
                reason: "domain in static policy allowlist".into(),
                was_escalated: false,
                resolved_scope: None,
            };
        }

        // 2. Runtime allowlist (Narc-approved earlier this session).
        if self.runtime_allowed_domain_pub(&req.domain) {
            return Verdict {
                allowed: true,
                reason: "domain approved by Safehouse Narc (runtime allowlist)".into(),
                was_escalated: false,
                resolved_scope: None,
            };
        }

        // 3. Auto-approve globs from the policy's access_requests
        //    config. Legacy escape hatch for common package registries.
        if policy.access_requests.should_auto_approve_domain(&req.domain) {
            return Verdict {
                allowed: true,
                reason: "domain in policy auto_approve_domains".into(),
                was_escalated: false,
                resolved_scope: None,
            };
        }

        // 4. Escalate to Safehouse Narc. With no Narc wired in, fall
        //    straight to deny — same fail-closed shape as today's
        //    HTTP path.
        let Some(narc) = self.narc_client() else {
            return Verdict {
                allowed: false,
                reason: format!("{} is not in the network allowlist and no Narc arbiter is configured", req.domain),
                was_escalated: false,
                resolved_scope: None,
            };
        };

        // Use the policy's identity metadata if the caller didn't
        // supply one (the existing HTTP path does it this way too).
        let operator_id = if req.operator_id.is_empty() {
            policy.metadata.operator_id.clone()
        } else {
            req.operator_id.clone()
        };
        let bead_id = if req.bead_id.is_empty() {
            policy.metadata.bead_id.clone()
        } else {
            req.bead_id.clone()
        };

        let judge_request = JudgeRequest {
            kind: JudgeKind::Network,
            operator_id,
            bead_id,
            phase: policy.metadata.phase.clone(),
            resource: req.domain.clone(),
            detail: Some(req.path.clone()),
            task_summary: None,
            agent_reason: None,
        };
        let decision = narc.judge(&judge_request).await;
        match decision.decision {
            Decision::Approve => {
                let glob = decision.add_to_allowlist_glob.clone().unwrap_or_else(|| req.domain.clone());
                let ttl = decision
                    .cache_ttl_seconds.map_or_else(|| std::time::Duration::from_secs(3600), std::time::Duration::from_secs);
                self.push_runtime_allow_pub(glob, ttl);
                Verdict {
                    allowed: true,
                    reason: format!("Narc approved ({:.2}): {}", decision.confidence, decision.reason),
                    was_escalated: true,
                    // The existing HTTP path doesn't surface
                    // resolved_scope; populate when iter-3a's pb
                    // changes thread it through.
                    resolved_scope: None,
                }
            }
            Decision::Deny => Verdict {
                allowed: false,
                reason: format!("Narc denied: {}", decision.reason),
                was_escalated: true,
                resolved_scope: None,
            },
            Decision::Ask | Decision::EscalateToHuman => Verdict {
                allowed: false,
                reason: format!("Narc {} (fail closed): {}", decision.decision_label(), decision.reason),
                was_escalated: true,
                resolved_scope: None,
            },
        }
    }

    async fn check_tool(&self, req: ToolReq) -> Verdict {
        let policy = self.policy().load();
        let allowed = policy.tools.can_use(&req.tool_name);
        let reason = if allowed {
            "tool in allowlist".to_string()
        } else if policy.tools.deny.contains(&req.tool_name) {
            format!("{} is explicitly denied", req.tool_name)
        } else {
            format!("{} is not in the tool allowlist", req.tool_name)
        };
        Verdict {
            allowed,
            reason,
            was_escalated: false,
            resolved_scope: None,
        }
    }

    async fn check_cli(&self, req: CliReq) -> Verdict {
        // Minimal port for iter-3b: the existing HTTP check_cli does
        // a dangerous-pattern check + readonly-fs check + Narc
        // escalation. The full version replicates that. For now we
        // hit the local rule-engine-equivalent and skip the LLM
        // escalation — full parity lands in iter-3f when we wire
        // up the production NarcClient.
        let policy = self.policy().load();
        let dangerous = is_dangerous_cli(&req.command);
        let writable = policy.filesystem.writable;
        if dangerous && !writable {
            return Verdict {
                allowed: false,
                reason: "dangerous command on a read-only filesystem".into(),
                was_escalated: false,
                resolved_scope: None,
            };
        }
        // Safe + writable → allow. Dangerous + writable → allow but
        // surface as "needs Narc" (caller can re-check via /check/cli
        // with full escalation if it wants). This matches the HTTP
        // path's structural shape for iter-3b.
        Verdict {
            allowed: true,
            reason: if dangerous {
                "command flagged dangerous; writable fs permits"
            } else {
                "command not flagged dangerous"
            }
            .into(),
            was_escalated: false,
            resolved_scope: None,
        }
    }

    async fn check_file(&self, req: FileReq) -> Verdict {
        // Mirror check_write's logic for FileAccess::Write. Read +
        // Execute fall through to "allowed" for now — the existing
        // HTTP surface doesn't have file-read gating yet.
        if !matches!(req.access, FileAccess::Write) {
            return Verdict {
                allowed: true,
                reason: format!("{:?} access not gated in v1", req.access),
                was_escalated: false,
                resolved_scope: None,
            };
        }

        let policy = self.policy().load();
        if !policy.filesystem.writable {
            return Verdict {
                allowed: false,
                reason: "filesystem is read-only in this phase".into(),
                was_escalated: false,
                resolved_scope: None,
            };
        }

        // Normalize and check mount boundary. Pull the helpers from
        // server.rs — they're not exported, so duplicate the tiny
        // logic. (Dedupe in cleanup.)
        let canonical = normalize_guest_path(&req.path);
        let in_mount = policy.mounts.iter().any(|m| path_starts_with(&canonical, &m.guest_path));
        if !in_mount {
            return Verdict {
                allowed: false,
                reason: format!("path {canonical} is outside the writable mounts (sandbox boundary)"),
                was_escalated: false,
                resolved_scope: None,
            };
        }

        match policy.is_guest_path_denied(&req.path) {
            Ok(true) => Verdict {
                allowed: false,
                reason: format!("{} matches a filesystem deny pattern", req.path),
                was_escalated: false,
                resolved_scope: None,
            },
            Ok(false) => Verdict {
                allowed: true,
                reason: "path is allowed".into(),
                was_escalated: false,
                resolved_scope: None,
            },
            // Fail-open on glob errors — same as HTTP handler.
            Err(e) => Verdict {
                allowed: true,
                reason: format!("glob error (allowing): {e}"),
                was_escalated: false,
                resolved_scope: None,
            },
        }
    }

    async fn reload_policy(&self) -> ReloadResult {
        // The existing PolicyHolder hot-reloads via notify; reload
        // through the gRPC surface is a future feature. For iter-3b
        // we return a "not supported" result without erroring so
        // callers don't have to special-case.
        let policy = self.policy().load();
        ReloadResult {
            reloaded: false,
            error: "policy reload over gRPC not yet implemented; PolicyHolder hot-reloads via notify".into(),
            // Surface the current counts so callers still get
            // something useful. Allowlists in practice have <100
            // entries; the u32 caps anything that wouldn't fit.
            network_allow_hosts: u32::try_from(policy.network.allow.len()).unwrap_or(u32::MAX),
            tools_allow: u32::try_from(policy.tools.allow.len()).unwrap_or(u32::MAX),
            bash_allow_patterns: 0,
        }
    }

    async fn policy_summary(&self) -> PolicySummary {
        let policy = self.policy().load();
        let allow_hosts: Vec<String> = policy.network.allow.iter().map(|r| r.domain.clone()).collect();
        let allow_tools = policy.tools.allow.clone();
        // bash patterns aren't in the legacy Policy shape yet —
        // they're a future iteration. Leave empty.
        let allow_bash_patterns = Vec::new();
        PolicySummary {
            allow_hosts,
            allow_tools,
            allow_bash_patterns,
            runtime_allowlist_size: u32::try_from(self.runtime_allowlist_size()).unwrap_or(u32::MAX),
            user_grants_path: String::new(),
            project_grants_path: String::new(),
        }
    }
}

// --- Helpers duplicated from server.rs ---
//
// These are pure functions; the dedup is a Phase-4 cleanup task.

fn normalize_guest_path(path: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    for component in path.split('/') {
        match component {
            "" | "." => continue,
            ".." => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    let mut s = String::with_capacity(path.len());
    if path.starts_with('/') {
        s.push('/');
    }
    s.push_str(&out.join("/"));
    if s.is_empty() {
        s.push('/');
    }
    s
}

fn path_starts_with(path: &str, prefix: &str) -> bool {
    let p = normalize_guest_path(prefix);
    if path == p {
        return true;
    }
    let with_slash = if p.ends_with('/') { p } else { format!("{p}/") };
    path.starts_with(&with_slash)
}

fn is_dangerous_cli(command: &str) -> bool {
    let dangerous_prefixes = ["rm ", "chmod ", "chown ", "git push", "git reset", "mv ", "cp "];
    let cmd_lower = command.to_lowercase();
    dangerous_prefixes.iter().any(|p| cmd_lower.starts_with(p))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::grpc::{Checker, NetworkReq};
    use crate::{Negotiator, PolicyHolder};
    use smooth_policy::Policy;
    use std::sync::Arc;
    use std::time::Duration;
    use tempfile::TempDir;
    use tower::service_fn;

    const POLICY_TOML: &str = r#"
[metadata]
operator_id = "op-1"
bead_id = "pearl-1"
phase = "execute"

[auth]
token = "test-token"

[network]
[[network.allow]]
domain = "api.llmgateway.io"

[filesystem]
writable = true
deny_patterns = []

[[mounts]]
guest_path = "/workspace"
host_path = "/tmp/work"

[tools]
allow = ["grep", "read_file"]
deny = ["delete_repo"]

[beads]

[mcp]

[access_requests]
enabled = true
auto_approve_domains = ["registry.npmjs.org"]
auto_approve_tools = []
"#;

    fn build_app_state() -> Arc<AppState> {
        let policy = Policy::from_toml(POLICY_TOML).expect("parse policy");
        let holder = PolicyHolder::from_policy(policy);
        let negotiator = Negotiator::new("http://127.0.0.1:1/no-leader", holder.clone());
        Arc::new(AppState::new(holder, negotiator))
    }

    async fn build_uds_client(uds_path: std::path::PathBuf) -> crate::pb::wonk_client::WonkClient<tonic::transport::Channel> {
        let channel = tonic::transport::Endpoint::try_from("http://[::]:50051")
            .unwrap()
            .connect_with_connector(service_fn(move |_: tonic::transport::Uri| {
                let path = uds_path.clone();
                async move {
                    let stream = tokio::net::UnixStream::connect(path).await?;
                    Ok::<_, std::io::Error>(hyper_util::rt::TokioIo::new(stream))
                }
            }))
            .await
            .expect("connect UDS");
        crate::pb::wonk_client::WonkClient::new(channel)
    }

    #[tokio::test]
    async fn check_network_static_allowlist_via_grpc() {
        let tmp = TempDir::new().unwrap();
        let sock = tmp.path().join("wonk.sock");
        let state = build_app_state();
        let _server = crate::grpc::serve_uds(state, sock.clone()).unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;

        let mut client = build_uds_client(sock).await;
        let resp = client
            .check_network(tonic::Request::new(crate::pb::CheckNetworkRequest {
                domain: "api.llmgateway.io".into(),
                path: "/v1/chat".into(),
                method: "POST".into(),
                operator_id: "op-1".into(),
                bead_id: "pearl-1".into(),
            }))
            .await
            .unwrap()
            .into_inner();
        assert!(resp.allowed);
        assert!(resp.reason.contains("static"));
        assert!(!resp.was_escalated);
    }

    #[tokio::test]
    async fn check_network_auto_approve_domain_via_grpc() {
        let tmp = TempDir::new().unwrap();
        let sock = tmp.path().join("wonk.sock");
        let state = build_app_state();
        let _server = crate::grpc::serve_uds(state, sock.clone()).unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;

        let mut client = build_uds_client(sock).await;
        let resp = client
            .check_network(tonic::Request::new(crate::pb::CheckNetworkRequest {
                domain: "registry.npmjs.org".into(),
                path: "/foo".into(),
                method: "GET".into(),
                operator_id: "op-1".into(),
                bead_id: "pearl-1".into(),
            }))
            .await
            .unwrap()
            .into_inner();
        assert!(resp.allowed);
        assert!(resp.reason.contains("auto_approve"));
    }

    #[tokio::test]
    async fn check_network_unknown_domain_denied_without_narc() {
        let tmp = TempDir::new().unwrap();
        let sock = tmp.path().join("wonk.sock");
        let state = build_app_state();
        let _server = crate::grpc::serve_uds(state, sock.clone()).unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;

        let mut client = build_uds_client(sock).await;
        let resp = client
            .check_network(tonic::Request::new(crate::pb::CheckNetworkRequest {
                domain: "attacker.example".into(),
                path: "/".into(),
                method: "GET".into(),
                operator_id: "op-1".into(),
                bead_id: "pearl-1".into(),
            }))
            .await
            .unwrap()
            .into_inner();
        assert!(!resp.allowed);
        assert!(resp.reason.contains("not in the network allowlist"));
    }

    #[tokio::test]
    async fn check_tool_allowed_vs_denied_vs_unknown() {
        let tmp = TempDir::new().unwrap();
        let sock = tmp.path().join("wonk.sock");
        let state = build_app_state();
        let _server = crate::grpc::serve_uds(state, sock.clone()).unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;

        let mut client = build_uds_client(sock).await;

        let ok = client
            .check_tool(tonic::Request::new(crate::pb::CheckToolRequest {
                tool_name: "grep".into(),
                operator_id: "op".into(),
                bead_id: "pearl".into(),
            }))
            .await
            .unwrap()
            .into_inner();
        assert!(ok.allowed);

        let denied = client
            .check_tool(tonic::Request::new(crate::pb::CheckToolRequest {
                tool_name: "delete_repo".into(),
                operator_id: "op".into(),
                bead_id: "pearl".into(),
            }))
            .await
            .unwrap()
            .into_inner();
        assert!(!denied.allowed);
        assert!(denied.reason.contains("explicitly denied"));

        let unknown = client
            .check_tool(tonic::Request::new(crate::pb::CheckToolRequest {
                tool_name: "novel-tool".into(),
                operator_id: "op".into(),
                bead_id: "pearl".into(),
            }))
            .await
            .unwrap()
            .into_inner();
        assert!(!unknown.allowed);
        assert!(unknown.reason.contains("not in the tool allowlist"));
    }

    #[tokio::test]
    async fn check_file_write_inside_mount_allowed() {
        let tmp = TempDir::new().unwrap();
        let sock = tmp.path().join("wonk.sock");
        let state = build_app_state();
        let _server = crate::grpc::serve_uds(state, sock.clone()).unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;

        let mut client = build_uds_client(sock).await;
        let resp = client
            .check_file(tonic::Request::new(crate::pb::CheckFileRequest {
                path: "/workspace/src/main.rs".into(),
                access: crate::pb::AccessKind::Write as i32,
                operator_id: "op".into(),
                bead_id: "pearl".into(),
            }))
            .await
            .unwrap()
            .into_inner();
        assert!(resp.allowed);
    }

    #[tokio::test]
    async fn check_file_write_outside_mount_denied() {
        let tmp = TempDir::new().unwrap();
        let sock = tmp.path().join("wonk.sock");
        let state = build_app_state();
        let _server = crate::grpc::serve_uds(state, sock.clone()).unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;

        let mut client = build_uds_client(sock).await;
        let resp = client
            .check_file(tonic::Request::new(crate::pb::CheckFileRequest {
                path: "/etc/passwd".into(),
                access: crate::pb::AccessKind::Write as i32,
                operator_id: "op".into(),
                bead_id: "pearl".into(),
            }))
            .await
            .unwrap()
            .into_inner();
        assert!(!resp.allowed);
        assert!(resp.reason.contains("sandbox boundary"));
    }

    #[tokio::test]
    async fn check_file_write_traversal_denied() {
        let tmp = TempDir::new().unwrap();
        let sock = tmp.path().join("wonk.sock");
        let state = build_app_state();
        let _server = crate::grpc::serve_uds(state, sock.clone()).unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;

        let mut client = build_uds_client(sock).await;
        // `/workspace/../etc/passwd` normalizes to `/etc/passwd`,
        // which lies outside the workspace mount.
        let resp = client
            .check_file(tonic::Request::new(crate::pb::CheckFileRequest {
                path: "/workspace/../etc/passwd".into(),
                access: crate::pb::AccessKind::Write as i32,
                operator_id: "op".into(),
                bead_id: "pearl".into(),
            }))
            .await
            .unwrap()
            .into_inner();
        assert!(!resp.allowed);
    }

    #[tokio::test]
    async fn check_cli_dangerous_on_writable_fs_allowed_with_flag() {
        // Iter-3b semantics: dangerous + writable fs → allow but
        // flagged. The HTTP handler in server.rs does Narc
        // escalation in this branch; iter-3b takes the simpler
        // shape pending iter-3f.
        let tmp = TempDir::new().unwrap();
        let sock = tmp.path().join("wonk.sock");
        let state = build_app_state();
        let _server = crate::grpc::serve_uds(state, sock.clone()).unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;

        let mut client = build_uds_client(sock).await;
        let resp = client
            .check_cli(tonic::Request::new(crate::pb::CheckCliRequest {
                command: "rm -rf /workspace/build".into(),
                cwd: "/workspace".into(),
                operator_id: "op".into(),
                bead_id: "pearl".into(),
            }))
            .await
            .unwrap()
            .into_inner();
        assert!(resp.allowed);
        assert!(resp.reason.contains("dangerous"));
    }

    #[tokio::test]
    async fn get_policy_summary_lists_allowlist() {
        let tmp = TempDir::new().unwrap();
        let sock = tmp.path().join("wonk.sock");
        let state = build_app_state();
        let _server = crate::grpc::serve_uds(state, sock.clone()).unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;

        let mut client = build_uds_client(sock).await;
        let summary = client
            .get_policy_summary(tonic::Request::new(crate::pb::GetPolicySummaryRequest {}))
            .await
            .unwrap()
            .into_inner();
        assert!(summary.allow_hosts.iter().any(|h| h == "api.llmgateway.io"));
        assert!(summary.allow_tools.iter().any(|t| t == "grep"));
        assert_eq!(summary.runtime_allowlist_size, 0);
    }

    /// Sanity check: the Checker impl on AppState can also be
    /// driven through the trait directly (without spawning a
    /// server). Verifies the trait-vs-inherent boundary is correct.
    #[tokio::test]
    async fn checker_trait_drives_appstate_directly() {
        let state = build_app_state();
        let verdict = state
            .check_network(NetworkReq {
                domain: "api.llmgateway.io".into(),
                path: "/v1".into(),
                method: "GET".into(),
                operator_id: "op".into(),
                bead_id: "pearl".into(),
            })
            .await;
        assert!(verdict.allowed);
    }
}

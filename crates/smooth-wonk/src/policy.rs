use std::path::{Path, PathBuf};
use std::sync::Arc;

use arc_swap::ArcSwap;
use notify::{Event, EventKind, RecommendedWatcher, Watcher};
use smooth_policy::{EnterprisePolicy, Policy};

/// Thread-safe, hot-reloadable policy holder.
/// Uses `ArcSwap` for lock-free reads and `notify` for filesystem watching.
///
/// If an `EnterprisePolicy` is set, it is automatically merged into every
/// policy on load, update, and hot-reload. Enterprise rules cannot be
/// overridden by per-task policies.
#[derive(Clone)]
pub struct PolicyHolder {
    inner: Arc<ArcSwap<Policy>>,
    enterprise: Option<Arc<EnterprisePolicy>>,
    #[allow(dead_code)]
    path: PathBuf,
}

impl PolicyHolder {
    /// Load a policy from disk and start watching for changes.
    ///
    /// # Errors
    /// Returns error if the file cannot be read or the TOML is invalid.
    pub fn load_and_watch(path: &str) -> anyhow::Result<Self> {
        let path = PathBuf::from(path);
        let mut policy = load_policy_file(&path)?;
        let enterprise = EnterprisePolicy::load_default().map(Arc::new);
        if let Some(ref ep) = enterprise {
            policy.merge_enterprise(ep);
            tracing::info!("enterprise policy applied on load");
        }
        let inner = Arc::new(ArcSwap::from_pointee(policy));

        let holder = Self {
            inner: Arc::clone(&inner),
            enterprise: enterprise.clone(),
            path: path.clone(),
        };

        // Start file watcher in background
        let watcher_inner = Arc::clone(&inner);
        let watcher_enterprise = enterprise.clone();
        let watch_path = path.clone();
        tokio::spawn(async move {
            if let Err(e) = watch_policy_file(watch_path, watcher_inner, watcher_enterprise).await {
                tracing::error!(error = %e, "policy watcher failed");
            }
        });

        tracing::info!(path = %path.display(), "policy loaded");
        Ok(holder)
    }

    /// Create a `PolicyHolder` from an in-memory policy (for testing or boardroom).
    #[allow(dead_code)]
    pub fn from_policy(policy: Policy) -> Self {
        Self {
            inner: Arc::new(ArcSwap::from_pointee(policy)),
            enterprise: None,
            path: PathBuf::new(),
        }
    }

    /// Create a `PolicyHolder` with an enterprise policy overlay.
    pub fn from_policy_with_enterprise(mut policy: Policy, enterprise: EnterprisePolicy) -> Self {
        policy.merge_enterprise(&enterprise);
        Self {
            enterprise: Some(Arc::new(enterprise)),
            inner: Arc::new(ArcSwap::from_pointee(policy)),
            path: PathBuf::new(),
        }
    }

    /// Load the current policy (lock-free read).
    pub fn load(&self) -> Arc<Policy> {
        self.inner.load_full()
    }

    /// Manually update the policy (used by negotiation when leader pushes new policy).
    /// Enterprise rules are re-applied automatically.
    pub fn update(&self, mut policy: Policy) {
        if let Some(ref ep) = self.enterprise {
            policy.merge_enterprise(ep);
        }
        self.inner.store(Arc::new(policy));
        tracing::info!("policy updated via negotiation (enterprise rules re-applied)");
    }
}

fn load_policy_file(path: &Path) -> anyhow::Result<Policy> {
    let contents = std::fs::read_to_string(path)?;
    Ok(Policy::from_toml(&contents)?)
}

async fn watch_policy_file(path: PathBuf, inner: Arc<ArcSwap<Policy>>, enterprise: Option<Arc<EnterprisePolicy>>) -> anyhow::Result<()> {
    let (tx, mut rx) = tokio::sync::mpsc::channel(16);

    let mut watcher = RecommendedWatcher::new(
        move |res: Result<Event, notify::Error>| {
            if let Ok(event) = res {
                if matches!(event.kind, EventKind::Modify(_) | EventKind::Create(_)) {
                    let _ = tx.blocking_send(());
                }
            }
        },
        notify::Config::default(),
    )?;

    // Watch the parent directory (some editors do atomic writes via rename)
    let watch_dir = path.parent().unwrap_or_else(|| Path::new("."));
    watcher.watch(watch_dir, notify::RecursiveMode::NonRecursive)?;

    tracing::info!(path = %path.display(), "watching for policy changes");

    while rx.recv().await.is_some() {
        // Small delay to let atomic writes complete
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        match load_policy_file(&path) {
            Ok(mut policy) => {
                if let Some(ref ep) = enterprise {
                    policy.merge_enterprise(ep);
                }
                inner.store(Arc::new(policy));
                tracing::info!(path = %path.display(), "policy hot-reloaded (enterprise rules applied)");
            }
            Err(e) => {
                tracing::warn!(error = %e, "failed to reload policy, keeping current");
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    const TEST_POLICY: &str = r#"
[metadata]
operator_id = "test-op"
bead_id = "test-bead"
phase = "execute"

[auth]
token = "smth_op_test_token"
leader_url = "http://localhost:4400"

[network]
[[network.allow]]
domain = "openrouter.ai"

[filesystem]
deny_patterns = ["*.env"]
writable = true

[tools]
allow = ["code_search"]
"#;

    #[test]
    fn from_policy_creates_holder() {
        let policy = Policy::from_toml(TEST_POLICY).expect("parse");
        let holder = PolicyHolder::from_policy(policy);
        let loaded = holder.load();
        assert_eq!(loaded.metadata.operator_id, "test-op");
    }

    #[test]
    fn update_replaces_policy() {
        let policy = Policy::from_toml(TEST_POLICY).expect("parse");
        let holder = PolicyHolder::from_policy(policy);

        // Update with a modified policy
        let modified = TEST_POLICY.replace("test-op", "updated-op");
        let new_policy = Policy::from_toml(&modified).expect("parse");
        holder.update(new_policy);

        let loaded = holder.load();
        assert_eq!(loaded.metadata.operator_id, "updated-op");
    }

    #[test]
    fn load_policy_from_file() {
        let dir = tempfile::tempdir().expect("tmpdir");
        let path = dir.path().join("policy.toml");
        let mut file = std::fs::File::create(&path).expect("create");
        file.write_all(TEST_POLICY.as_bytes()).expect("write");

        let policy = load_policy_file(&path).expect("load");
        assert_eq!(policy.metadata.operator_id, "test-op");
        assert!(policy.network.is_allowed("openrouter.ai", "/anything"));
    }

    #[test]
    fn load_policy_file_not_found() {
        let result = load_policy_file(Path::new("/nonexistent/policy.toml"));
        assert!(result.is_err());
    }

    #[test]
    fn load_policy_invalid_toml() {
        let dir = tempfile::tempdir().expect("tmpdir");
        let path = dir.path().join("bad.toml");
        std::fs::write(&path, "this is not valid toml {{{").expect("write");
        let result = load_policy_file(&path);
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn load_and_watch_from_file() {
        let dir = tempfile::tempdir().expect("tmpdir");
        let path = dir.path().join("policy.toml");
        std::fs::write(&path, TEST_POLICY).expect("write");

        let holder = PolicyHolder::load_and_watch(path.to_str().expect("path")).expect("load");
        let policy = holder.load();
        assert_eq!(policy.metadata.operator_id, "test-op");
        assert!(policy.network.is_allowed("openrouter.ai", "/zen"));
    }

    #[test]
    fn enterprise_policy_blocks_network_on_create() {
        let policy = Policy::from_toml(TEST_POLICY).expect("parse");
        assert!(policy.network.is_allowed("openrouter.ai", "/zen"));

        let enterprise = EnterprisePolicy {
            network: smooth_policy::EnterpriseNetworkPolicy {
                deny_domains: vec!["openrouter.ai".to_string()],
            },
            ..Default::default()
        };

        let holder = PolicyHolder::from_policy_with_enterprise(policy, enterprise);
        let loaded = holder.load();
        assert!(!loaded.network.is_allowed("openrouter.ai", "/zen"));
    }

    #[test]
    fn enterprise_policy_reapplied_on_update() {
        let policy = Policy::from_toml(TEST_POLICY).expect("parse");
        let enterprise = EnterprisePolicy {
            tools: smooth_policy::EnterpriseToolsPolicy {
                deny: vec!["code_search".to_string()],
            },
            ..Default::default()
        };

        let holder = PolicyHolder::from_policy_with_enterprise(policy, enterprise);
        assert!(!holder.load().tools.can_use("code_search"));

        // Even if someone pushes a policy that re-allows code_search,
        // enterprise should block it
        let new_policy = Policy::from_toml(TEST_POLICY).expect("parse");
        assert!(new_policy.tools.can_use("code_search")); // allowed before merge
        holder.update(new_policy);
        assert!(!holder.load().tools.can_use("code_search")); // still blocked!
    }
}

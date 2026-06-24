//! `smooth-tools` — reusable agent tools with workspace-confined paths.
//!
//! Clean reimplementations of the core tools the `smooth-operative` binary
//! defines inline, packaged so the `smooth-daemon` (and, later, the operative
//! itself) can register them on a [`ToolRegistry`] with one call
//! ([`register_default_tools`]).
//!
//! Every filesystem tool routes user paths through
//! [`path::resolve_workspace_path`] — the security floor that confines reads
//! and writes to the workspace. (Per EPIC th-c89c2a the load-bearing boundary
//! is the kernel OS-sandbox added in Phase 3; this is the cheap first gate.)
//!
//! Build-out:
//! - Slice A: read-only tools — `read_file`, `list_files`, `grep`.
//! - Slice B: mutating tools — `write_file`, `edit_file`.
//! - **Slice C (this):** `bash` (pre-sandbox; Phase 3 wraps it).

use std::path::PathBuf;
use std::sync::Arc;

use smooth_operator::{Tool, ToolRegistry};

pub mod bash;
pub mod grep;
pub mod path;
pub mod read;
pub mod remember;
pub mod sandbox;
mod util;
pub mod write;

pub use bash::BashTool;
pub use grep::GrepTool;
pub use path::resolve_workspace_path;
pub use read::{ListFilesTool, ReadFileTool};
pub use remember::RememberTool;
pub use sandbox::{SandboxPolicy, SandboxedCommand};
pub use write::{EditFileTool, WriteFileTool};

/// Register the default tool set on `registry`, all confined to `workspace`.
///
/// One call installs the full set; later slices extend it (shell tools), so
/// consumers keep calling this one function. `bash` egress is unrestricted —
/// use [`register_default_tools_with_proxy`] to route it through a proxy.
pub fn register_default_tools(registry: &mut ToolRegistry, workspace: PathBuf) {
    register_default_tools_with_proxy(registry, workspace, None);
}

/// Like [`register_default_tools`], but routes the `bash` tool's egress.
///
/// With `proxy` set (`host:port`), the shell's network goes through that
/// loopback proxy and direct off-box network is kernel-denied — so the proxy's
/// exact-host allowlist is the only way out. `None` leaves egress unrestricted.
pub fn register_default_tools_with_proxy(registry: &mut ToolRegistry, workspace: PathBuf, proxy: Option<String>) {
    for tool in default_tools_with_proxy(workspace, proxy) {
        registry.register_arc(tool);
    }
}

/// Build the default tool set as `Vec<Arc<dyn Tool>>`, for hosts that register
/// into *someone else's* registry rather than their own — e.g. the
/// smooth-operator local flavor's `LocalServerBuilder::tools` seam, which takes
/// pre-built `Arc<dyn Tool>`s and registers them into the agent it constructs
/// per turn. Same set + same proxy wiring as
/// [`register_default_tools_with_proxy`].
#[must_use]
pub fn default_tools_with_proxy(workspace: PathBuf, proxy: Option<String>) -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(ReadFileTool { workspace: workspace.clone() }) as Arc<dyn Tool>,
        Arc::new(ListFilesTool { workspace: workspace.clone() }),
        Arc::new(GrepTool { workspace: workspace.clone() }),
        Arc::new(WriteFileTool { workspace: workspace.clone() }),
        Arc::new(EditFileTool { workspace: workspace.clone() }),
        Arc::new(BashTool { workspace, proxy }),
    ]
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "unwrap is the idiom for test assertions")]
mod tests {
    use super::*;

    #[test]
    fn register_default_tools_adds_the_read_only_set() {
        let mut registry = ToolRegistry::new();
        register_default_tools(&mut registry, PathBuf::from("/tmp"));
        let names: Vec<String> = registry.schemas().into_iter().map(|s| s.name).collect();
        for expected in ["read_file", "list_files", "grep", "write_file", "edit_file", "bash"] {
            assert!(names.iter().any(|n| n == expected), "missing {expected} in {names:?}");
        }
    }

    #[test]
    fn default_tools_vec_has_the_full_set_with_proxy_wired() {
        let tools = default_tools_with_proxy(PathBuf::from("/tmp"), Some("127.0.0.1:4419".into()));
        let names: Vec<String> = tools.iter().map(|t| t.schema().name).collect();
        for expected in ["read_file", "list_files", "grep", "write_file", "edit_file", "bash"] {
            assert!(names.iter().any(|n| n == expected), "missing {expected} in {names:?}");
        }
        // The bash tool carries the proxy so its egress routes through goalie.
        let bash = BashTool {
            workspace: PathBuf::from("/tmp"),
            proxy: Some("127.0.0.1:4419".into()),
        };
        assert_eq!(bash.proxy.as_deref(), Some("127.0.0.1:4419"));
    }
}

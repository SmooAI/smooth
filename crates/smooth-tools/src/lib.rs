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

use smooth_operator::ToolRegistry;

pub mod bash;
pub mod grep;
pub mod path;
pub mod read;
pub mod sandbox;
mod util;
pub mod write;

pub use bash::BashTool;
pub use grep::GrepTool;
pub use path::resolve_workspace_path;
pub use read::{ListFilesTool, ReadFileTool};
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
    registry.register(ReadFileTool { workspace: workspace.clone() });
    registry.register(ListFilesTool { workspace: workspace.clone() });
    registry.register(GrepTool { workspace: workspace.clone() });
    registry.register(WriteFileTool { workspace: workspace.clone() });
    registry.register(EditFileTool { workspace: workspace.clone() });
    registry.register(BashTool { workspace, proxy });
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
}

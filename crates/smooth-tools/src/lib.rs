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
//! - **Slice A (this):** read-only tools — `read_file`, `list_files`, `grep`.
//! - Slice B: mutating tools — `write_file`, `edit_file`.
//! - Slice C: `bash` (pre-sandbox; Phase 3 wraps it).

use std::path::PathBuf;

use smooth_operator::ToolRegistry;

pub mod grep;
pub mod path;
pub mod read;
mod util;

pub use grep::GrepTool;
pub use path::resolve_workspace_path;
pub use read::{ListFilesTool, ReadFileTool};

/// Register the default tool set on `registry`, all confined to `workspace`.
///
/// Slice A registers the read-only tools; mutating + shell tools are added here
/// as later slices land, so consumers keep calling this one function.
pub fn register_default_tools(registry: &mut ToolRegistry, workspace: PathBuf) {
    registry.register(ReadFileTool { workspace: workspace.clone() });
    registry.register(ListFilesTool { workspace: workspace.clone() });
    registry.register(GrepTool { workspace });
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
        for expected in ["read_file", "list_files", "grep"] {
            assert!(names.iter().any(|n| n == expected), "missing {expected} in {names:?}");
        }
    }
}

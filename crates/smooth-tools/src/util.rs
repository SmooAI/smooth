//! Small argument-parsing and path helpers shared by the tools.

use std::path::Path;

use serde_json::Value;

/// Render a **workspace-relative** path with forward slashes on every platform.
///
/// Every tool that reports a relative path to the model routes through this, so
/// one file has one spelling regardless of host — the model reads these back
/// into `read_file`, and `src/main.rs` vs `src\main.rs` is a difference it
/// should never have to reason about. It also keeps backslashes out of strings
/// that get embedded in JSON tool results.
///
/// The replace is Windows-only on purpose: `\` is a legal character in a Unix
/// filename, so rewriting it there would corrupt a real path.
///
/// Absolute paths are deliberately NOT run through this — they stay native so
/// they can be handed straight back to the OS.
#[must_use]
pub fn to_slash(relative: &Path) -> String {
    let s = relative.to_string_lossy();
    if cfg!(target_os = "windows") {
        s.replace('\\', "/")
    } else {
        s.into_owned()
    }
}

/// Extract a required string parameter, or a descriptive error.
///
/// # Errors
/// Returns an error if `key` is absent or not a string.
pub fn req_str(args: &Value, key: &str) -> anyhow::Result<String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| anyhow::anyhow!("missing required string parameter `{key}`"))
}

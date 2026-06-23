//! Small argument-parsing helpers shared by the tools.

use serde_json::Value;

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

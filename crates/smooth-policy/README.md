# smooth-policy

TOML-based access control policy types for AI agent sandboxes. Defines the security boundary for network access, filesystem permissions, tool usage, and port exposure in Microsandbox operator VMs.

## Key Types

- **`Policy`** -- Top-level policy with metadata, auth, and all sub-policies
- **`NetworkPolicy`** -- Domain allowlists with path and method restrictions
- **`FilesystemPolicy`** -- Glob-based deny patterns and write control
- **`ToolsPolicy`** -- Tool allowlists, denylists, and confirmation requirements
- **`PortPolicy`** -- Exposed port definitions with protocol and visibility
- **`McpPolicy`** -- MCP server access control
- **`BeadsPolicy`** -- Work item access scoping

## Usage

```rust
use smooth_policy::Policy;

let toml = r#"
[metadata]
operator_id = "op-abc123"
phase = "execution"

[auth]
token = "secret-token"

[[network.allow]]
domain = "api.openai.com"
path = "/v1/*"
methods = ["POST"]

[[network.allow]]
domain = "*.npmjs.org"

[filesystem]
deny_patterns = ["*.env", ".ssh/*", "*.pem"]
writable = true

[tools]
allow = ["read_file", "write_file", "bash"]
deny = ["rm_rf"]
require_confirmation = ["bash"]
"#;

let policy = Policy::from_toml(toml).expect("valid policy");

// Check network access
assert!(policy.network.is_allowed("api.openai.com", "/v1/chat/completions"));
assert!(!policy.network.is_allowed("evil.com", "/"));

// Check filesystem access
assert!(policy.is_guest_path_denied("/workspace/.env").unwrap());
assert!(!policy.is_guest_path_denied("/workspace/src/main.rs").unwrap());

// Round-trip to TOML
let serialized = policy.to_toml().expect("serializes");
println!("{serialized}");
```

## License

MIT

## Links

- [GitHub](https://github.com/SmooAI/smooth)
- [crates.io](https://crates.io/crates/smooth-policy)

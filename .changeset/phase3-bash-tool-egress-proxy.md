---
'smooai-smooth-tools': patch
---

Phase 3 (EPIC th-c89c2a): give the `bash` tool an optional egress proxy.
`BashTool` gains a `proxy: Option<String>` field, and a new
`register_default_tools_with_proxy(registry, workspace, proxy)` installs the
default tool set with the shell's egress routed through a loopback proxy
(`SandboxPolicy::with_proxy`) — direct off-box network kernel-denied, so the
proxy's exact-host allowlist is the only way out. `register_default_tools`
is unchanged (egress unrestricted). This is the injection point the daemon
uses to wire its goalie proxy into agent shell commands.

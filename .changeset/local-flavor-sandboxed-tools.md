---
'smooai-smooth-daemon': minor
'smooai-smooth-tools': minor
---

EPIC th-c89c2a: the operator local flavor now runs with the daemon's
kernel-sandboxed tools. `serve_local_flavor` builds the workspace-confined
fs/grep set + an OS-sandboxed `bash` (egress routed through the goalie proxy
when `SMOOTH_EGRESS_ALLOWLIST` is set) and installs them via the operator's
`LocalServerBuilder::tools` seam — so the agent the operator runs per turn can
act on the workspace under the same kernel-enforced security the bespoke daemon
used. New `smooth_tools::default_tools_with_proxy(workspace, proxy) ->
Vec<Arc<dyn Tool>>` (the registry helper now shares it via `register_arc`); the
egress-proxy start is factored into a reusable `start_egress_proxy()`. Workspace
defaults to the daemon's cwd, overridable with `SMOOTH_WORKSPACE`.

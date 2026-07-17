---
"@smooai/smooth": minor
---

Big Smooth daemon: install the smooth-operator engine's `DenyPolicy`-backed
permission gate and retire the duplicate in-daemon `AutoModeHook`
(th-daemon-denypolicy).

- **Engine pins → core 1.7.0**: `smooth-operator` bumped `0.16.2 → 1.7.0`
  (crates.io), and `smooth-operator-server`/`-svc` re-pinned to
  `9db9d319287e2ebcd3ab027e39971a0f51ef5b67` (the branch built against core 1.7.0
  that also carries the `LocalServer::tool_hooks` seam). One core version resolves
  across the whole workspace. The daemon's own surface (tools, LlmConfig,
  ToolProvider, narc) needed no migration — the bump compiled clean.
- **PermissionHook first, narc second**: the daemon now installs the engine's
  `permission::PermissionHook` (core 1.7.0) as the FIRST tool_hook — running in
  `AutoMode::Bypass` (allow benign, block dangerous) layered with an embedded
  declarative `DenyPolicy` circuit-breaker deny tier — then `NarcHook` second. The
  mode honors an explicit `SMOOTH_AUTO_MODE` override (`ask`/`accept-edits`/`deny`)
  and defaults to Bypass when unset.
- **DenyPolicy from the retired starter deny-list**: the dangerous-op deny-list
  the old `AutoModeHook` shipped (`sudo`/`su`/`shutdown`/`reboot`/`dd`/`mkfs`/…
  bash bins + `/etc/**`, `**/.ssh/**`, `**/.smooth/auth/**`, … path writes) is
  re-expressed as the engine's `DenyPolicy` TOML, embedded as the daemon default.
  Bypass + deny-policy preserves the allow-benign / block-dangerous posture: a
  policy match is a hard circuit-breaker Bypass cannot downgrade.
- **Removed**: `crates/smooth-daemon/src/hooks/auto_mode.rs` and its `mod`/re-exports.
  `smooth_policy::auto_mode` (used by `smooth-tools`/`th`'s `permissions` command)
  is untouched — only the daemon HOOK is gone.

//! Engine `ToolHook`s that re-home the daemon's security model onto the
//! smooth-operator local flavor (EPIC th-c89c2a; pearls th-3119e3 + th-515a13).
//!
//! When the microVM stack was removed, the per-VM Wonk/Goalie/Narc cast went
//! with it. The operator's `LocalServer` exposes a `.tool_hooks(Vec<Arc<dyn
//! ToolHook>>)` seam that installs host hooks on **every** per-turn tool
//! registry, *before* the per-agent auth gate — so a host hook gets first say
//! on every call. The daemon installs two hooks on that seam:
//!
//! - The **permission gate** — the ENGINE'S `permission::PermissionHook` (core
//!   1.7.0), built in [`crate::operator::permission_hook`]. It runs in
//!   [`AutoMode::Bypass`](smooth_operator::permission::AutoMode::Bypass) (allow
//!   benign, block dangerous) layered with the daemon's embedded declarative
//!   [`DenyPolicy`](smooth_operator::deny_policy::DenyPolicy) circuit-breaker
//!   deny tier. Installed FIRST so a policy deny short-circuits before
//!   surveillance or the tool itself runs. (th-daemon-denypolicy retired the
//!   daemon's duplicate in-tree `AutoModeHook` in favor of this engine hook.)
//! - [`NarcHook`] — **surveillance**. Regex detectors (secret exfiltration,
//!   prompt injection, dangerous shell ops) on tool arguments, escalating
//!   ambiguous hits to an LLM judge (the daemon's fast model), and **redacting
//!   detected secrets out of tool results** via the mutable `post_call` seam.
//!   Installed SECOND (after the permission gate).
//!
//! Wiring order in [`crate::operator::serve_local_flavor`] is
//! `vec![Arc::new(permission_hook()), Arc::new(narc)]` — permission gate, then
//! narc.

pub mod narc;

pub use narc::NarcHook;

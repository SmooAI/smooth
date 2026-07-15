//! Engine `ToolHook`s that re-home the daemon's security model onto the
//! smooth-operator local flavor (EPIC th-c89c2a; pearls th-3119e3 + th-515a13).
//!
//! When the microVM stack was removed, the per-VM Wonk/Goalie/Narc cast went
//! with it. The operator's `LocalServer` exposes a `.tool_hooks(Vec<Arc<dyn
//! ToolHook>>)` seam that installs host hooks on **every** per-turn tool
//! registry, *before* the per-agent auth gate — so a host hook gets first say
//! on every call. This module re-homes the two load-bearing pieces onto that
//! seam:
//!
//! - [`AutoModeHook`] — the **permission gate**. Runs the deterministic
//!   [`smooth_policy::auto_mode`] rule engine (allow / deny / ask) and, since
//!   the daemon has no interactive approval queue yet, **fails closed on `ask`**
//!   (th-1f7fd7 wires the real queue). Installed FIRST so a deny short-circuits
//!   before surveillance or the tool itself runs.
//! - [`NarcHook`] — **surveillance**. Regex detectors (secret exfiltration,
//!   prompt injection, dangerous shell ops) on tool arguments, escalating
//!   ambiguous hits to an LLM judge (the daemon's fast model), and **redacting
//!   detected secrets out of tool results** via the mutable `post_call` seam.
//!   Installed SECOND (after the permission gate).
//!
//! Wiring order in [`crate::operator::serve_local_flavor`] is
//! `vec![Arc::new(auto_mode), Arc::new(narc)]` — permission gate, then narc.

pub mod auto_mode;
pub mod narc;

pub use auto_mode::{AutoMode, AutoModeHook};
pub use narc::NarcHook;

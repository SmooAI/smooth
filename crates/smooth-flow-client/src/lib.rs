//! SmoothFlow client logic (th-3e6020): the pure rules every SmoothFlow client
//! follows, written once. See `docs/Architecture/SmoothFlow-Client-Spec.md`.
//!
//! The Rust desktop app (Linux/Windows, GPUI) uses this crate directly. The
//! other clients — SmoothFlow for Mac (Swift), iOS (Swift) and Android
//! (Kotlin) — keep their own implementations but replay the **conformance
//! vectors** this crate writes to `spec/vectors/*.json`, so none of them can
//! drift from the spec without a failing test. See [`vectors`].
//!
//! Nothing here does I/O or knows about a UI toolkit.

pub mod attention;
pub mod close;
pub mod diff;
pub mod directory;
pub mod fleet;
pub mod gate;
pub mod harness;
pub mod keymap;
pub mod pane;
pub mod replay;
pub mod session;
pub mod surfaces;
pub mod title;
pub mod vectors;

pub use session::{Session, SessionState};

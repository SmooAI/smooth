//! SmoothFlow engine (epic th-6ac036, lane A th-7f0af3).
//!
//! The ONLY state holder for flow sessions: agents and shells run under one
//! long-lived tmux server, their PTY bytes stream to attached clients over
//! the daemon's `/api/flow/ws`, harness hooks drive state, and a supervision
//! tick keeps agents alive (resume on death, usage-limit scheduling,
//! duplicate-resume guard). Hosted by `smooth-daemon`; `th flow`, the macOS
//! app and the phones are dumb views over it.

pub mod diff;
pub mod doctor;
pub mod engine;
pub mod harness;
pub mod harness_conformance;
pub mod harness_draft;
pub mod harness_validate;
pub mod hook_auth;
pub mod host;
pub mod infer;
pub mod limit;
pub mod pane_path;
pub mod proc;
pub mod protocol;
pub mod pty;
#[cfg(all(unix, feature = "pty-host"))]
pub mod pty_host;
pub mod repos;
pub mod scrape;
#[cfg(feature = "pty-host")]
pub mod session_host;
pub mod store;
pub mod tmux;
pub mod vocab;

pub use engine::{Engine, EngineConfig, HookReply, NewRequest};
pub use hook_auth::HookCaller;
pub use host::{HostKind, SessionHost, SessionRef, TmuxHost};
pub use infer::{infer, Inferred};
pub use protocol::{ClientFrame, CloseOutcome, DaemonInfo, Decision, HookEvent, ReplayReason, ServerFrame};
#[cfg(all(unix, feature = "pty-host"))]
pub use pty_host::PtyHost;
/// The session host's headless terminal, for callers (and tests) that
/// replay its snapshots.
#[cfg(feature = "pty-host")]
pub use smooth_flow_vt as vt;
pub use store::{Attention, FanOut, FlowStore, Session, SessionKind, SessionState};

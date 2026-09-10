//! SmoothFlow engine (epic th-6ac036, lane A th-7f0af3).
//!
//! The ONLY state holder for flow sessions: agents and shells run under one
//! long-lived tmux server, their PTY bytes stream to attached clients over
//! the daemon's `/api/flow/ws`, harness hooks drive state, and a supervision
//! tick keeps agents alive (resume on death, usage-limit scheduling,
//! duplicate-resume guard). Hosted by `smooth-daemon`; `th flow`, the macOS
//! app and the phones are dumb views over it.

pub mod engine;
pub mod harness;
pub mod harness_draft;
pub mod harness_validate;
pub mod limit;
pub mod proc;
pub mod protocol;
pub mod pty;
pub mod store;
pub mod tmux;

pub use engine::{Engine, EngineConfig, HookReply, NewRequest};
pub use protocol::{ClientFrame, CloseOutcome, DaemonInfo, Decision, HookEvent, ServerFrame};
pub use store::{Attention, FanOut, FlowStore, Session, SessionKind, SessionState};

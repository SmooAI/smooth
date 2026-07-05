//! Orchestrator — lightweight state reporter for the work loop.
//!
//! Historically this drove a 5-state machine (Idle → Scheduling →
//! Dispatching → Monitoring → Reviewing) that spun up per-task
//! microVMs via a `SandboxPool`. The microVM dispatch stack was
//! removed 2026-07 (pearl th-f4a801; see git history to resurrect) —
//! dispatch now runs in-process through the WebSocket `TaskStart`
//! handler (`server::dispatch_ws_task_direct`). What remains here is
//! the state/queue surface the status routes, TUI, and web dashboard
//! still read.

use std::collections::HashMap;

use serde::Serialize;
use smooth_pearls::PearlStore;
use tokio::sync::broadcast;

use crate::events::ServerEvent;

/// Worker phases.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Phase {
    Assess,
    Plan,
    Orchestrate,
    Execute,
    Finalize,
}

impl Phase {
    /// Timeout in seconds for each phase.
    #[must_use]
    pub const fn timeout_seconds(self) -> u64 {
        match self {
            Self::Assess => 30 * 60,
            Self::Plan => 10 * 60,
            Self::Orchestrate => 15 * 60,
            Self::Execute => 90 * 60,
            Self::Finalize => 15 * 60,
        }
    }

    /// Next phase in the lifecycle.
    #[must_use]
    pub const fn next(self) -> Option<Self> {
        match self {
            Self::Assess => Some(Self::Plan),
            Self::Plan => Some(Self::Orchestrate),
            Self::Orchestrate => Some(Self::Execute),
            Self::Execute => Some(Self::Finalize),
            Self::Finalize => None, // Done → review
        }
    }
}

impl std::fmt::Display for Phase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Assess => write!(f, "assess"),
            Self::Plan => write!(f, "plan"),
            Self::Orchestrate => write!(f, "orchestrate"),
            Self::Execute => write!(f, "execute"),
            Self::Finalize => write!(f, "finalize"),
        }
    }
}

/// Orchestrator state.
#[derive(Debug)]
pub enum OrchestratorState {
    /// Waiting for work.
    Idle,
    /// Picking ready beads and prioritizing.
    Scheduling { ready_beads: Vec<String> },
    /// Assigning beads to operators.
    Dispatching { assignments: HashMap<String, String> },
    /// Watching active operators.
    Monitoring,
    /// Reviewing completed work.
    Reviewing { bead_id: String },
}

/// The orchestrator tracks work-loop state for status/TUI/web.
pub struct Orchestrator {
    pub state: OrchestratorState,
    pub pearl_store: PearlStore,
    /// Max concurrent operators — surfaced by the status routes.
    pub max_operators: usize,
    /// Beads the loop has seen complete this session.
    pub completed_beads: Vec<String>,
    /// Broadcast sender for forwarding events to TUI/web clients.
    pub event_tx: Option<broadcast::Sender<ServerEvent>>,
}

impl Orchestrator {
    /// Create a new orchestrator.
    pub fn new(max_operators: usize, pearl_store: PearlStore) -> Self {
        Self {
            state: OrchestratorState::Idle,
            pearl_store,
            max_operators,
            completed_beads: Vec::new(),
            event_tx: None,
        }
    }

    /// Set the broadcast sender for forwarding operator events to connected clients.
    pub fn with_event_tx(mut self, event_tx: broadcast::Sender<ServerEvent>) -> Self {
        self.event_tx = Some(event_tx);
        self
    }

    /// Nudge the orchestrator back to Idle so status readers see a
    /// settled state. Dispatch is external (WebSocket-driven) now, so
    /// this is purely a state reset.
    pub fn nudge(&mut self) {
        self.state = OrchestratorState::Idle;
    }

    /// Broadcast a `ServerEvent` to all connected clients (if event_tx is set).
    pub fn broadcast(&self, event: ServerEvent) {
        if let Some(tx) = &self.event_tx {
            let _ = tx.send(event);
        }
    }

    /// Number of currently-tracked active operators. Dispatch runs
    /// in-process now, so the orchestrator no longer owns worker
    /// handles — always 0.
    #[must_use]
    pub fn active_worker_count(&self) -> usize {
        0
    }

    /// Get current state name.
    #[must_use]
    pub fn state_name(&self) -> &str {
        match &self.state {
            OrchestratorState::Idle => "idle",
            OrchestratorState::Scheduling { .. } => "scheduling",
            OrchestratorState::Dispatching { .. } => "dispatching",
            OrchestratorState::Monitoring => "monitoring",
            OrchestratorState::Reviewing { .. } => "reviewing",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_store() -> Option<PearlStore> {
        let tmp = tempfile::tempdir().unwrap();
        let dolt_dir = tmp.path().join("dolt");
        match PearlStore::init(&dolt_dir) {
            Ok(store) => {
                std::mem::forget(tmp);
                Some(store)
            }
            Err(_) => None, // Dolt binary not available — skip
        }
    }

    #[test]
    fn test_phase_timeout() {
        assert_eq!(Phase::Assess.timeout_seconds(), 30 * 60);
        assert_eq!(Phase::Execute.timeout_seconds(), 90 * 60);
    }

    #[test]
    fn test_phase_next() {
        assert_eq!(Phase::Assess.next(), Some(Phase::Plan));
        assert_eq!(Phase::Plan.next(), Some(Phase::Orchestrate));
        assert_eq!(Phase::Finalize.next(), None);
    }

    #[test]
    fn test_phase_display() {
        assert_eq!(format!("{}", Phase::Execute), "execute");
    }

    #[test]
    fn test_orchestrator_new() {
        let Some(store) = test_store() else { return };
        let orch = Orchestrator::new(3, store);
        assert_eq!(orch.state_name(), "idle");
        assert_eq!(orch.active_worker_count(), 0);
        assert_eq!(orch.max_operators, 3);
    }

    #[test]
    fn test_orchestrator_nudge_resets_to_idle() {
        let Some(store) = test_store() else { return };
        let mut orch = Orchestrator::new(3, store);
        orch.state = OrchestratorState::Monitoring;
        orch.nudge();
        assert_eq!(orch.state_name(), "idle");
    }

    #[test]
    fn test_orchestrator_broadcast_forwards_events() {
        let Some(store) = test_store() else { return };
        let (tx, mut rx) = broadcast::channel::<ServerEvent>(16);
        let orch = Orchestrator::new(3, store).with_event_tx(tx);
        orch.broadcast(ServerEvent::TokenDelta {
            task_id: "bead-1".into(),
            content: "hello from operator".into(),
        });
        let received = rx.try_recv().expect("should receive broadcast event");
        let json = serde_json::to_string(&received).expect("serialize");
        assert!(json.contains("TokenDelta"));
        assert!(json.contains("hello from operator"));
    }
}

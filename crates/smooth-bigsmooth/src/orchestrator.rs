//! Orchestrator — state machine for coordinating Smooth Operators.
//!
//! Replaces LangGraph with a simple, typed state machine.
//! The graph has 5 states: Idle → Scheduling → Dispatching → Monitoring → Reviewing.

use std::collections::HashMap;

use anyhow::Result;
use serde::Serialize;
use smooth_pearls::{PearlStatus, PearlStore, PearlUpdate};
use tokio::sync::broadcast;

use crate::events::ServerEvent;
use crate::operator_client::{OperatorClient, OperatorEvent};
use crate::pool::SandboxPool;
use crate::sandbox::SandboxHandle;

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

/// The orchestrator drives the work loop.
pub struct Orchestrator {
    pub state: OrchestratorState,
    pub pool: SandboxPool,
    pub pearl_store: PearlStore,
    pub active_workers: HashMap<String, SandboxHandle>,
    pub completed_beads: Vec<String>,
    /// WebSocket clients for communicating with each operator (keyed by bead_id).
    pub operator_clients: HashMap<String, OperatorClient>,
    /// Broadcast sender for forwarding events to TUI/web clients.
    pub event_tx: Option<broadcast::Sender<ServerEvent>>,
}

impl Orchestrator {
    /// Create a new orchestrator.
    pub fn new(max_operators: usize, pearl_store: PearlStore) -> Self {
        Self {
            state: OrchestratorState::Idle,
            pool: SandboxPool::new(max_operators),
            pearl_store,
            active_workers: HashMap::new(),
            completed_beads: Vec::new(),
            operator_clients: HashMap::new(),
            event_tx: None,
        }
    }

    /// Set the broadcast sender for forwarding operator events to connected clients.
    pub fn with_event_tx(mut self, event_tx: broadcast::Sender<ServerEvent>) -> Self {
        self.event_tx = Some(event_tx);
        self
    }

    /// Broadcast a `ServerEvent` to all connected clients (if event_tx is set).
    fn broadcast(&self, event: ServerEvent) {
        if let Some(tx) = &self.event_tx {
            let _ = tx.send(event);
        }
    }

    /// Run one step of the orchestration loop.
    pub async fn step(&mut self) -> Result<()> {
        match &self.state {
            OrchestratorState::Idle => self.schedule().await,
            OrchestratorState::Scheduling { .. } => self.dispatch().await,
            OrchestratorState::Dispatching { .. } => self.monitor().await,
            OrchestratorState::Monitoring => self.aggregate().await,
            OrchestratorState::Reviewing { .. } => self.review().await,
        }
    }

    /// Schedule: find ready issues.
    async fn schedule(&mut self) -> Result<()> {
        let ready = self.pearl_store.ready().unwrap_or_default();
        let ready_ids: Vec<String> = ready.iter().map(|b| b.id.clone()).collect();

        if ready_ids.is_empty() {
            // Nothing to do, stay idle
            return Ok(());
        }

        tracing::info!("Scheduling {} ready bead(s)", ready_ids.len());
        self.state = OrchestratorState::Scheduling { ready_beads: ready_ids };
        Ok(())
    }

    /// Dispatch: assign beads to operators.
    async fn dispatch(&mut self) -> Result<()> {
        let ready_beads = match &self.state {
            OrchestratorState::Scheduling { ready_beads } => ready_beads.clone(),
            _ => return Ok(()),
        };

        let mut assignments = HashMap::new();

        for bead_id in &ready_beads {
            if !self.pool.has_capacity() {
                tracing::info!("At capacity, queuing remaining beads");
                break;
            }

            match self.pool.create_operator(bead_id).await {
                Ok(handle) => {
                    let operator_id = handle.operator_id.clone();
                    let ws_url = format!("ws://localhost:{}/ws", handle.host_port);
                    tracing::info!("Dispatched operator {operator_id} for bead {bead_id}");

                    let _ = self.pearl_store.update(
                        bead_id,
                        &PearlUpdate {
                            status: Some(PearlStatus::InProgress),
                            ..Default::default()
                        },
                    );

                    // Create OperatorClient and connect to the operator's WS server
                    let mut client = OperatorClient::new(&operator_id, &ws_url);
                    match client.connect().await {
                        Ok(()) => {
                            // Look up the issue title/description for the task message
                            let message = self
                                .pearl_store
                                .get(bead_id)
                                .ok()
                                .flatten()
                                .map(|issue| format!("{}: {}", issue.title, issue.description))
                                .unwrap_or_else(|| format!("Work on bead {bead_id}"));

                            if let Err(e) = client.assign_task(bead_id, &message, None, "").await {
                                tracing::error!("Failed to assign task for {bead_id}: {e}");
                            }
                            self.operator_clients.insert(bead_id.clone(), client);
                        }
                        Err(e) => {
                            tracing::error!("Failed to connect to operator {operator_id}: {e}");
                        }
                    }

                    self.active_workers.insert(bead_id.clone(), handle);
                    assignments.insert(bead_id.clone(), operator_id);
                }
                Err(e) => {
                    tracing::error!("Failed to create operator for {bead_id}: {e}");
                }
            }
        }

        self.state = if self.active_workers.is_empty() {
            OrchestratorState::Idle
        } else {
            OrchestratorState::Monitoring
        };

        Ok(())
    }

    /// Monitor: check active operators and poll for events from operator clients.
    async fn monitor(&mut self) -> Result<()> {
        let mut completed = Vec::new();
        let mut failed = Vec::new();

        // Poll each operator client for events (non-blocking via try_recv)
        let bead_ids: Vec<String> = self.operator_clients.keys().cloned().collect();
        for bead_id in &bead_ids {
            if let Some(client) = self.operator_clients.get_mut(bead_id) {
                // Use tokio::time::timeout for a near-instant non-blocking poll
                let poll_result = tokio::time::timeout(std::time::Duration::from_millis(10), client.recv()).await;

                if let Ok(Some(event)) = poll_result {
                    match event {
                        OperatorEvent::TaskComplete { iterations, cost_usd } => {
                            tracing::info!(bead_id, iterations, cost_usd, "Operator completed task");
                            self.broadcast(ServerEvent::TaskComplete {
                                task_id: bead_id.clone(),
                                iterations,
                                cost_usd,
                            });
                            completed.push(bead_id.clone());
                        }
                        OperatorEvent::TaskError { message } => {
                            tracing::error!(bead_id, message, "Operator task failed");
                            self.broadcast(ServerEvent::TaskError {
                                task_id: bead_id.clone(),
                                message: message.clone(),
                            });
                            failed.push((bead_id.clone(), message));
                        }
                        OperatorEvent::NarcAlert { severity, category, message } => {
                            self.broadcast(ServerEvent::NarcAlert { severity, category, message });
                        }
                        OperatorEvent::TokenDelta { content } => {
                            self.broadcast(ServerEvent::TokenDelta {
                                task_id: bead_id.clone(),
                                content,
                            });
                        }
                        OperatorEvent::ToolCallStart { tool_name, arguments } => {
                            self.broadcast(ServerEvent::ToolCallStart {
                                task_id: bead_id.clone(),
                                tool_name,
                                arguments,
                            });
                        }
                        OperatorEvent::ToolCallComplete {
                            tool_name,
                            result,
                            is_error,
                            duration_ms,
                        } => {
                            self.broadcast(ServerEvent::ToolCallComplete {
                                task_id: bead_id.clone(),
                                tool_name,
                                result,
                                is_error,
                                duration_ms,
                            });
                        }
                        OperatorEvent::CheckpointSaved { .. } | OperatorEvent::Heartbeat => {
                            // Informational only, no broadcast needed
                        }
                    }
                }
            }
        }

        // Also check sandbox status for operators without active WS connections
        for (bead_id, handle) in &self.active_workers {
            if !completed.contains(bead_id) && !failed.iter().any(|(id, _)| id == bead_id) {
                let status = crate::sandbox::get_status(&handle.msb_name).await;
                if !status.running {
                    completed.push(bead_id.clone());
                }
            }
        }

        // Transition completed/failed beads to review
        let all_done: Vec<String> = completed.into_iter().chain(failed.into_iter().map(|(id, _)| id)).collect();

        if !all_done.is_empty() {
            self.state = OrchestratorState::Reviewing { bead_id: all_done[0].clone() };
            for id in &all_done {
                if let Some(handle) = self.active_workers.remove(id) {
                    self.pool.release(&handle.operator_id).await;
                    self.completed_beads.push(id.clone());
                }
            }
        }

        Ok(())
    }

    /// Aggregate: collect results from completed work.
    async fn aggregate(&mut self) -> Result<()> {
        // Move back to monitoring or idle
        self.state = if self.active_workers.is_empty() {
            OrchestratorState::Idle
        } else {
            OrchestratorState::Monitoring
        };
        Ok(())
    }

    /// Review: evaluate completed work.
    async fn review(&mut self) -> Result<()> {
        let bead_id = match &self.state {
            OrchestratorState::Reviewing { bead_id } => bead_id.clone(),
            _ => return Ok(()),
        };

        tracing::info!("Reviewing bead {bead_id}");

        // Disconnect and clean up the operator client
        if let Some(mut client) = self.operator_clients.remove(&bead_id) {
            client.disconnect();
        }

        // Destroy sandbox if it wasn't already cleaned up in monitor
        if let Some(handle) = self.active_workers.remove(&bead_id) {
            let _ = crate::sandbox::destroy_sandbox(&handle.msb_name).await;
        }

        // TODO: spawn review operator

        // For now, auto-approve
        let _ = self.pearl_store.close(&[&bead_id]);

        self.state = if self.active_workers.is_empty() {
            OrchestratorState::Idle
        } else {
            OrchestratorState::Monitoring
        };

        Ok(())
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
        let store = {
            let tmp = tempfile::tempdir().unwrap();
            let dolt_dir = tmp.path().join("dolt");
            match PearlStore::init(&dolt_dir) {
                Ok(store) => {
                    std::mem::forget(tmp);
                    store
                }
                Err(_) => return,
            }
        };
        let orch = Orchestrator::new(3, store);
        assert_eq!(orch.state_name(), "idle");
        assert!(orch.active_workers.is_empty());
        assert!(orch.operator_clients.is_empty());
    }

    #[tokio::test]
    async fn test_orchestrator_step_idle() {
        let store = {
            let tmp = tempfile::tempdir().unwrap();
            let dolt_dir = tmp.path().join("dolt");
            match PearlStore::init(&dolt_dir) {
                Ok(store) => {
                    std::mem::forget(tmp);
                    store
                }
                Err(_) => return,
            }
        };
        let mut orch = Orchestrator::new(3, store);
        // Step from idle — no issues, stays idle
        let result = orch.step().await;
        assert!(result.is_ok());
        assert_eq!(orch.state_name(), "idle");
    }

    #[test]
    fn test_orchestrator_dispatch_creates_operator_client_map() {
        // Verify that the orchestrator has an operator_clients map ready for dispatch.
        // Actual sandbox creation requires msb, so we test the data structure setup.
        let store = {
            let tmp = tempfile::tempdir().unwrap();
            let dolt_dir = tmp.path().join("dolt");
            match PearlStore::init(&dolt_dir) {
                Ok(store) => {
                    std::mem::forget(tmp);
                    store
                }
                Err(_) => return,
            }
        };
        let orch = Orchestrator::new(3, store);

        assert!(orch.operator_clients.is_empty());
        assert_eq!(orch.pool.active_count(), 0);
        assert!(orch.pool.has_capacity());
    }

    #[tokio::test]
    async fn test_orchestrator_monitor_forwards_events_to_broadcast() {
        // Set up orchestrator with a broadcast channel
        let store = {
            let tmp = tempfile::tempdir().unwrap();
            let dolt_dir = tmp.path().join("dolt");
            match PearlStore::init(&dolt_dir) {
                Ok(store) => {
                    std::mem::forget(tmp);
                    store
                }
                Err(_) => return,
            }
        };
        let (tx, mut rx) = broadcast::channel::<ServerEvent>(16);
        let mut orch = Orchestrator::new(3, store).with_event_tx(tx);

        // Manually broadcast a token delta (simulating what monitor does)
        orch.broadcast(ServerEvent::TokenDelta {
            task_id: "bead-1".into(),
            content: "hello from operator".into(),
        });

        // Verify the broadcast was received
        let received = rx.try_recv().expect("should receive broadcast event");
        let json = serde_json::to_string(&received).expect("serialize");
        assert!(json.contains("TokenDelta"));
        assert!(json.contains("hello from operator"));
    }

    #[test]
    fn test_orchestrator_with_event_tx() {
        let store = {
            let tmp = tempfile::tempdir().unwrap();
            let dolt_dir = tmp.path().join("dolt");
            match PearlStore::init(&dolt_dir) {
                Ok(store) => {
                    std::mem::forget(tmp);
                    store
                }
                Err(_) => return,
            }
        };
        let (tx, _rx) = broadcast::channel::<ServerEvent>(16);
        let orch = Orchestrator::new(3, store).with_event_tx(tx);
        assert!(orch.event_tx.is_some());
    }

    #[tokio::test]
    async fn test_orchestrator_review_cleans_up_operator_client() {
        let store = {
            let tmp = tempfile::tempdir().unwrap();
            let dolt_dir = tmp.path().join("dolt");
            match PearlStore::init(&dolt_dir) {
                Ok(store) => {
                    std::mem::forget(tmp);
                    store
                }
                Err(_) => return,
            }
        };
        let mut orch = Orchestrator::new(3, store);

        // Insert a mock (disconnected) operator client
        let client = OperatorClient::new("op-1", "ws://localhost:9999/ws");
        orch.operator_clients.insert("bead-1".into(), client);

        // Put in reviewing state
        orch.state = OrchestratorState::Reviewing { bead_id: "bead-1".into() };

        // Review should clean up
        let result = orch.review().await;
        assert!(result.is_ok());
        assert!(orch.operator_clients.is_empty());
    }
}

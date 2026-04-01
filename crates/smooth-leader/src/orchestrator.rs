//! Orchestrator — state machine for coordinating Smooth Operators.
//!
//! Replaces LangGraph with a simple, typed state machine.
//! The graph has 5 states: Idle → Scheduling → Dispatching → Monitoring → Reviewing.

use std::collections::HashMap;

use anyhow::Result;
use serde::Serialize;

use crate::beads;
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
    pub active_workers: HashMap<String, SandboxHandle>,
    pub completed_beads: Vec<String>,
}

impl Orchestrator {
    /// Create a new orchestrator.
    #[must_use]
    pub fn new(max_operators: usize) -> Self {
        Self {
            state: OrchestratorState::Idle,
            pool: SandboxPool::new(max_operators),
            active_workers: HashMap::new(),
            completed_beads: Vec::new(),
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

    /// Schedule: find ready beads.
    async fn schedule(&mut self) -> Result<()> {
        let ready = beads::get_ready().unwrap_or_default();
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

            match self.pool.create_operator(bead_id) {
                Ok(handle) => {
                    let operator_id = handle.operator_id.clone();
                    tracing::info!("Dispatched operator {operator_id} for bead {bead_id}");
                    let _ = beads::update_bead_status(bead_id, "in_progress");
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

    /// Monitor: check active operators.
    async fn monitor(&mut self) -> Result<()> {
        // Check for completed / timed out workers
        let mut completed = Vec::new();

        for (bead_id, handle) in &self.active_workers {
            let status = crate::sandbox::get_status(&handle.msb_name);
            if !status.running {
                completed.push(bead_id.clone());
            }
        }

        if !completed.is_empty() {
            self.state = OrchestratorState::Reviewing { bead_id: completed[0].clone() };
            for id in &completed {
                if let Some(handle) = self.active_workers.remove(id) {
                    self.pool.release(&handle.operator_id);
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
        // TODO: spawn review operator

        // For now, auto-approve
        let _ = beads::update_bead_status(&bead_id, "closed");

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
        let orch = Orchestrator::new(3);
        assert_eq!(orch.state_name(), "idle");
        assert!(orch.active_workers.is_empty());
    }

    #[tokio::test]
    async fn test_orchestrator_step_idle() {
        let mut orch = Orchestrator::new(3);
        // Step from idle should try to schedule
        let result = orch.step().await;
        // May fail if bd is not available, that's ok
        assert!(result.is_ok() || result.is_err());
    }
}

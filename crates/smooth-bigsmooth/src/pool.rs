//! Sandbox pool — capacity management and operator lifecycle.

use std::collections::HashMap;

use anyhow::Result;

use crate::sandbox::{self, SandboxConfig, SandboxHandle};

/// Manages sandbox capacity and tracks active operators.
pub struct SandboxPool {
    max_operators: usize,
    active: HashMap<String, SandboxHandle>,
    next_port: u16,
}

impl SandboxPool {
    /// Create a new pool with the given max concurrency.
    #[must_use]
    pub fn new(max_operators: usize) -> Self {
        Self {
            max_operators,
            active: HashMap::new(),
            next_port: 14096,
        }
    }

    /// Check if there's capacity for another operator.
    #[must_use]
    pub fn has_capacity(&self) -> bool {
        self.active.len() < self.max_operators
    }

    /// Number of active operators.
    #[must_use]
    pub fn active_count(&self) -> usize {
        self.active.len()
    }

    /// Max operators allowed.
    #[must_use]
    pub fn max_concurrency(&self) -> usize {
        self.max_operators
    }

    /// Create a new operator for a bead.
    pub fn create_operator(&mut self, bead_id: &str) -> Result<SandboxHandle> {
        if !self.has_capacity() {
            anyhow::bail!("At capacity ({} / {})", self.active.len(), self.max_operators);
        }

        let config = SandboxConfig {
            bead_id: bead_id.into(),
            ..SandboxConfig::default()
        };

        let port = self.next_port;
        self.next_port += 1;

        let handle = sandbox::create_sandbox(&config, port)?;
        self.active.insert(handle.operator_id.clone(), handle.clone());

        Ok(handle)
    }

    /// Release an operator (after completion or kill).
    pub fn release(&mut self, operator_id: &str) {
        if let Some(handle) = self.active.remove(operator_id) {
            let _ = sandbox::destroy_sandbox(&handle.msb_name);
        }
    }

    /// Get all active handles.
    #[must_use]
    pub fn list_active(&self) -> Vec<&SandboxHandle> {
        self.active.values().collect()
    }

    /// Destroy all active sandboxes.
    pub fn drain(&mut self) {
        let ids: Vec<String> = self.active.keys().cloned().collect();
        for id in ids {
            self.release(&id);
        }
    }
}

impl Drop for SandboxPool {
    fn drop(&mut self) {
        self.drain();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pool_capacity() {
        let pool = SandboxPool::new(3);
        assert!(pool.has_capacity());
        assert_eq!(pool.active_count(), 0);
        assert_eq!(pool.max_concurrency(), 3);
    }
}

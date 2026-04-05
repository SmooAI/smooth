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
    ///
    /// # Errors
    ///
    /// Returns an error if the pool is at capacity or the underlying sandbox
    /// backend fails to boot the VM.
    pub async fn create_operator(&mut self, bead_id: &str) -> Result<SandboxHandle> {
        if !self.has_capacity() {
            anyhow::bail!("At capacity ({} / {})", self.active.len(), self.max_operators);
        }

        let config = SandboxConfig {
            bead_id: bead_id.into(),
            ..SandboxConfig::default()
        };

        let port = self.next_port;
        self.next_port += 1;

        let handle = sandbox::create_sandbox(&config, port).await?;
        self.active.insert(handle.operator_id.clone(), handle.clone());

        Ok(handle)
    }

    /// Release an operator (after completion or kill).
    ///
    /// Stops the underlying sandbox. Errors from the backend are logged but
    /// not returned, because callers treat release as best-effort cleanup.
    pub async fn release(&mut self, operator_id: &str) {
        if let Some(handle) = self.active.remove(operator_id) {
            if let Err(e) = sandbox::destroy_sandbox(&handle.msb_name).await {
                tracing::warn!(operator_id, error = %e, "release: failed to destroy sandbox");
            }
        }
    }

    /// Get all active handles.
    #[must_use]
    pub fn list_active(&self) -> Vec<&SandboxHandle> {
        self.active.values().collect()
    }

    /// Destroy all active sandboxes.
    pub async fn drain(&mut self) {
        let ids: Vec<String> = self.active.keys().cloned().collect();
        for id in ids {
            self.release(&id).await;
        }
    }
}

impl Drop for SandboxPool {
    /// On drop we cannot await async cleanup, so we fire off a detached task
    /// per active sandbox (if a tokio runtime is available) and clear the map.
    /// The VMs will be stopped best-effort. Production callers should call
    /// [`SandboxPool::drain`] explicitly during graceful shutdown.
    fn drop(&mut self) {
        let handles: Vec<SandboxHandle> = self.active.drain().map(|(_, h)| h).collect();
        if handles.is_empty() {
            return;
        }
        if let Ok(rt) = tokio::runtime::Handle::try_current() {
            rt.spawn(async move {
                for handle in handles {
                    if let Err(e) = sandbox::destroy_sandbox(&handle.msb_name).await {
                        tracing::warn!(operator_id = %handle.operator_id, error = %e, "drop: failed to destroy sandbox");
                    }
                }
            });
        } else {
            tracing::warn!(count = handles.len(), "SandboxPool dropped outside a tokio runtime; leaking sandbox handles");
        }
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

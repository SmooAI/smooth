//! Diver store — pearl lifecycle management backed by `PearlStore`.
//!
//! Diver makes pearl usage structural: every dispatched task gets a pearl,
//! operators can create sub-pearls, and completion closes the pearl. Cost
//! tracking and Jira sync are layered on top.

use std::sync::Mutex;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use smooth_pearls::{NewPearl, Pearl, PearlQuery, PearlStatus, PearlStore, PearlType, PearlUpdate, Priority};

/// Request to dispatch a task — Diver creates a pearl for it.
#[derive(Debug, Deserialize)]
pub struct DispatchRequest {
    /// Human-readable title for the task.
    pub title: String,
    /// Detailed description / user prompt.
    pub description: String,
    /// Pearl type: task, bug, feature, etc.
    #[serde(default = "default_type")]
    pub pearl_type: String,
    /// Priority 1-4.
    #[serde(default = "default_priority")]
    pub priority: u8,
    /// Optional parent pearl ID (for sub-pearls).
    pub parent_id: Option<String>,
    /// Labels to attach.
    #[serde(default)]
    pub labels: Vec<String>,
    /// Operator ID that will handle this task.
    pub operator_id: Option<String>,
}

fn default_type() -> String {
    "task".to_string()
}

const fn default_priority() -> u8 {
    2
}

/// Result of dispatching a task through Diver.
#[derive(Debug, Serialize, Deserialize)]
pub struct DispatchResult {
    /// The created pearl.
    pub pearl: Pearl,
    /// Jira ticket key, if one was created.
    pub jira_key: Option<String>,
}

/// Request to mark a task as complete.
#[derive(Debug, Deserialize)]
pub struct CompleteRequest {
    /// Pearl ID to close.
    pub pearl_id: String,
    /// Optional completion summary.
    pub summary: Option<String>,
    /// Final cost in USD.
    pub cost_usd: Option<f64>,
}

/// A cost entry for a pearl.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CostEntry {
    pub pearl_id: String,
    pub operator_id: String,
    pub cost_usd: f64,
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub model: String,
    pub timestamp: DateTime<Utc>,
}

/// A session message to be synced to Jira.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMessage {
    pub pearl_id: String,
    pub from: String,
    pub to: String,
    pub content: String,
    #[serde(default)]
    pub message_type: String,
}

/// The Diver store — wraps `PearlStore` with lifecycle management.
pub struct DiverStore {
    pearl_store: PearlStore,
    /// In-memory cost ledger (will move to Dolt table later).
    costs: Mutex<Vec<CostEntry>>,
    /// Pearl ID → Jira ticket key mapping.
    jira_keys: Mutex<std::collections::HashMap<String, String>>,
}

impl DiverStore {
    /// Create a new `DiverStore` wrapping an existing `PearlStore`.
    pub fn new(pearl_store: PearlStore) -> Self {
        Self {
            pearl_store,
            costs: Mutex::new(Vec::new()),
            jira_keys: Mutex::new(std::collections::HashMap::new()),
        }
    }

    /// Register a Jira key for a pearl.
    pub fn set_jira_key(&self, pearl_id: &str, jira_key: &str) {
        if let Ok(mut keys) = self.jira_keys.lock() {
            keys.insert(pearl_id.to_string(), jira_key.to_string());
        }
    }

    /// Get the Jira key for a pearl.
    pub fn jira_key(&self, pearl_id: &str) -> Option<String> {
        self.jira_keys.lock().ok().and_then(|keys| keys.get(pearl_id).cloned())
    }

    /// Access the underlying `PearlStore`.
    pub fn pearl_store(&self) -> &PearlStore {
        &self.pearl_store
    }

    /// Dispatch a task: create a pearl, optionally with a parent.
    pub fn dispatch(&self, req: &DispatchRequest) -> Result<DispatchResult> {
        let pearl_type = PearlType::from_str_loose(&req.pearl_type).unwrap_or(PearlType::Task);
        let priority = Priority::from_u8(req.priority).unwrap_or(Priority::Medium);

        let new = NewPearl {
            title: req.title.clone(),
            description: req.description.clone(),
            pearl_type,
            priority,
            assigned_to: req.operator_id.clone(),
            parent_id: req.parent_id.clone(),
            labels: req.labels.clone(),
        };

        let pearl = self.pearl_store.create(&new).context("diver: create pearl for dispatch")?;

        // Mark as in_progress immediately since it's being dispatched
        let update = PearlUpdate {
            status: Some(PearlStatus::InProgress),
            ..Default::default()
        };
        let pearl = self.pearl_store.update(&pearl.id, &update).context("diver: set pearl in_progress")?;

        tracing::info!(pearl_id = %pearl.id, title = %pearl.title, "diver: dispatched task");

        Ok(DispatchResult { pearl, jira_key: None })
    }

    /// Complete a task: close the pearl and record final cost.
    pub fn complete(&self, req: &CompleteRequest) -> Result<Pearl> {
        // Add completion comment if summary provided
        if let Some(ref summary) = req.summary {
            self.pearl_store.add_comment(&req.pearl_id, summary).context("diver: add completion summary")?;
        }

        // Record cost if provided
        if let Some(cost) = req.cost_usd {
            self.record_cost(CostEntry {
                pearl_id: req.pearl_id.clone(),
                operator_id: "diver".to_string(),
                cost_usd: cost,
                tokens_in: 0,
                tokens_out: 0,
                model: "final".to_string(),
                timestamp: Utc::now(),
            });
        }

        // Close the pearl
        self.pearl_store.close(&[&req.pearl_id]).context("diver: close pearl")?;
        let pearl = self.pearl_store.get(&req.pearl_id).context("diver: get closed pearl")?.unwrap_or_else(|| {
            // Shouldn't happen, but construct a minimal pearl
            Pearl {
                id: req.pearl_id.clone(),
                title: String::new(),
                description: String::new(),
                status: PearlStatus::Closed,
                pearl_type: PearlType::Task,
                priority: Priority::Medium,
                assigned_to: None,
                parent_id: None,
                labels: Vec::new(),
                created_at: Utc::now(),
                updated_at: Utc::now(),
                closed_at: Some(Utc::now()),
            }
        });

        tracing::info!(pearl_id = %pearl.id, "diver: completed task");

        Ok(pearl)
    }

    /// Create a sub-pearl (child of an existing pearl).
    pub fn create_sub_pearl(&self, parent_id: &str, title: &str, description: &str) -> Result<Pearl> {
        // Verify parent exists
        self.pearl_store
            .get(parent_id)
            .context("diver: get parent pearl")?
            .ok_or_else(|| anyhow::anyhow!("parent pearl {parent_id} not found"))?;

        let new = NewPearl {
            title: title.to_string(),
            description: description.to_string(),
            pearl_type: PearlType::Task,
            priority: Priority::Medium,
            assigned_to: None,
            parent_id: Some(parent_id.to_string()),
            labels: Vec::new(),
        };

        let pearl = self.pearl_store.create(&new).context("diver: create sub-pearl")?;
        tracing::info!(pearl_id = %pearl.id, parent = %parent_id, "diver: created sub-pearl");

        Ok(pearl)
    }

    /// Get a pearl by ID.
    pub fn get(&self, id: &str) -> Result<Option<Pearl>> {
        self.pearl_store.get(id)
    }

    /// List pearls with optional status filter.
    pub fn list(&self, status: Option<&str>) -> Result<Vec<Pearl>> {
        let query = match status {
            Some(s) => PearlQuery::new().with_status(PearlStatus::from_str_loose(s).unwrap_or(PearlStatus::Open)),
            None => PearlQuery::new(),
        };
        self.pearl_store.list(&query)
    }

    /// Get children of a pearl.
    pub fn children(&self, parent_id: &str) -> Result<Vec<Pearl>> {
        let all = self.pearl_store.list(&PearlQuery::new())?;
        Ok(all.into_iter().filter(|p| p.parent_id.as_deref() == Some(parent_id)).collect())
    }

    /// Record a cost entry.
    pub fn record_cost(&self, entry: CostEntry) {
        if let Ok(mut costs) = self.costs.lock() {
            costs.push(entry);
        }
    }

    /// Get total cost for a pearl (including sub-pearls).
    pub fn total_cost(&self, pearl_id: &str) -> f64 {
        let costs = self.costs.lock().unwrap_or_else(|e| e.into_inner());
        let mut total: f64 = costs.iter().filter(|c| c.pearl_id == pearl_id).map(|c| c.cost_usd).sum();

        // Include sub-pearl costs
        if let Ok(children) = self.children(pearl_id) {
            for child in &children {
                total += costs.iter().filter(|c| c.pearl_id == child.id).map(|c| c.cost_usd).sum::<f64>();
            }
        }

        total
    }

    /// Get all cost entries for a pearl.
    pub fn costs(&self, pearl_id: &str) -> Vec<CostEntry> {
        let costs = self.costs.lock().unwrap_or_else(|e| e.into_inner());
        costs.iter().filter(|c| c.pearl_id == pearl_id).cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_store() -> Option<DiverStore> {
        let tmp = tempfile::tempdir().ok()?;
        let dolt_dir = tmp.path().join("dolt");
        match PearlStore::init(&dolt_dir) {
            Ok(store) => {
                std::mem::forget(tmp); // keep temp dir alive
                Some(DiverStore::new(store))
            }
            Err(_) => None, // smooth-dolt binary not available
        }
    }

    #[test]
    fn dispatch_creates_in_progress_pearl() {
        let Some(store) = test_store() else { return };
        let req = DispatchRequest {
            title: "Build login page".to_string(),
            description: "Implement auth flow".to_string(),
            pearl_type: "task".to_string(),
            priority: 1,
            parent_id: None,
            labels: vec!["auth".to_string()],
            operator_id: Some("op-1".to_string()),
        };
        let result = store.dispatch(&req).expect("dispatch");
        assert_eq!(result.pearl.title, "Build login page");
        assert_eq!(result.pearl.status, PearlStatus::InProgress);
        assert_eq!(result.pearl.assigned_to.as_deref(), Some("op-1"));
    }

    #[test]
    fn complete_closes_pearl_with_summary() {
        let Some(store) = test_store() else { return };
        let dispatch = store
            .dispatch(&DispatchRequest {
                title: "Fix bug".to_string(),
                description: "".to_string(),
                pearl_type: "bug".to_string(),
                priority: 2,
                parent_id: None,
                labels: Vec::new(),
                operator_id: None,
            })
            .expect("dispatch");

        let completed = store
            .complete(&CompleteRequest {
                pearl_id: dispatch.pearl.id.clone(),
                summary: Some("Fixed the null pointer".to_string()),
                cost_usd: Some(0.05),
            })
            .expect("complete");

        assert_eq!(completed.status, PearlStatus::Closed);
    }

    #[test]
    fn sub_pearl_links_to_parent() {
        let Some(store) = test_store() else { return };
        let parent = store
            .dispatch(&DispatchRequest {
                title: "Parent task".to_string(),
                description: "".to_string(),
                pearl_type: "task".to_string(),
                priority: 2,
                parent_id: None,
                labels: Vec::new(),
                operator_id: None,
            })
            .expect("dispatch parent");

        let child = store.create_sub_pearl(&parent.pearl.id, "Sub task", "Detail").expect("create sub-pearl");
        assert_eq!(child.parent_id.as_deref(), Some(parent.pearl.id.as_str()));

        let children = store.children(&parent.pearl.id).expect("children");
        assert_eq!(children.len(), 1);
        assert_eq!(children[0].id, child.id);
    }

    #[test]
    fn sub_pearl_rejects_missing_parent() {
        let Some(store) = test_store() else { return };
        let result = store.create_sub_pearl("th-000000", "Orphan", "");
        assert!(result.is_err());
    }

    #[test]
    fn cost_tracking() {
        let Some(store) = test_store() else { return };
        let dispatch = store
            .dispatch(&DispatchRequest {
                title: "Costed task".to_string(),
                description: "".to_string(),
                pearl_type: "task".to_string(),
                priority: 2,
                parent_id: None,
                labels: Vec::new(),
                operator_id: None,
            })
            .expect("dispatch");

        store.record_cost(CostEntry {
            pearl_id: dispatch.pearl.id.clone(),
            operator_id: "op-1".to_string(),
            cost_usd: 0.03,
            tokens_in: 500,
            tokens_out: 200,
            model: "gpt-4o".to_string(),
            timestamp: Utc::now(),
        });
        store.record_cost(CostEntry {
            pearl_id: dispatch.pearl.id.clone(),
            operator_id: "op-1".to_string(),
            cost_usd: 0.02,
            tokens_in: 300,
            tokens_out: 100,
            model: "gpt-4o".to_string(),
            timestamp: Utc::now(),
        });

        let total = store.total_cost(&dispatch.pearl.id);
        assert!((total - 0.05).abs() < f64::EPSILON);

        let entries = store.costs(&dispatch.pearl.id);
        assert_eq!(entries.len(), 2);
    }

    #[test]
    fn total_cost_includes_sub_pearls() {
        let Some(store) = test_store() else { return };
        let parent = store
            .dispatch(&DispatchRequest {
                title: "Parent".to_string(),
                description: "".to_string(),
                pearl_type: "task".to_string(),
                priority: 2,
                parent_id: None,
                labels: Vec::new(),
                operator_id: None,
            })
            .expect("dispatch");
        let child = store.create_sub_pearl(&parent.pearl.id, "Child", "").expect("sub-pearl");

        store.record_cost(CostEntry {
            pearl_id: parent.pearl.id.clone(),
            operator_id: "op-1".to_string(),
            cost_usd: 0.10,
            tokens_in: 0,
            tokens_out: 0,
            model: "test".to_string(),
            timestamp: Utc::now(),
        });
        store.record_cost(CostEntry {
            pearl_id: child.id.clone(),
            operator_id: "op-2".to_string(),
            cost_usd: 0.05,
            tokens_in: 0,
            tokens_out: 0,
            model: "test".to_string(),
            timestamp: Utc::now(),
        });

        let total = store.total_cost(&parent.pearl.id);
        assert!((total - 0.15).abs() < f64::EPSILON);
    }

    #[test]
    fn list_with_status_filter() {
        let Some(store) = test_store() else { return };
        let d = store
            .dispatch(&DispatchRequest {
                title: "In progress".to_string(),
                description: "".to_string(),
                pearl_type: "task".to_string(),
                priority: 2,
                parent_id: None,
                labels: Vec::new(),
                operator_id: None,
            })
            .expect("dispatch");

        let in_progress = store.list(Some("in_progress")).expect("list");
        assert_eq!(in_progress.len(), 1);

        store
            .complete(&CompleteRequest {
                pearl_id: d.pearl.id,
                summary: None,
                cost_usd: None,
            })
            .expect("complete");

        let closed = store.list(Some("closed")).expect("list");
        assert_eq!(closed.len(), 1);
        let in_progress = store.list(Some("in_progress")).expect("list");
        assert!(in_progress.is_empty());
    }
}

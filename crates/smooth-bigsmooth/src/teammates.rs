//! Live-teammate registry + comment-tap broadcaster.
//!
//! Phase 4 of the plan (~/.claude/plans/sorted-orbiting-hummingbird.md):
//! the chat-agent and the web UI need a place to look up "what teammates
//! are running right now and what pearls are they on". The dispatch path
//! adds an entry on spawn, the comment-tap marks it idle when the teammate
//! posts `[IDLE]`, and we expose the registry over REST + a per-pearl
//! `TeammateChat` event stream the web UI subscribes to.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, RwLock};

use crate::events::ServerEvent;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeammateView {
    pub name: String,
    pub pearl_id: String,
    pub title: String,
    pub status: String, // "running" | "idle" | "ended"
    pub started_at: DateTime<Utc>,
    pub last_event_at: DateTime<Utc>,
}

#[derive(Default)]
pub struct OperatorRegistry {
    by_name: RwLock<HashMap<String, TeammateView>>,
}

impl OperatorRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a freshly-spawned teammate. The slug name must be unique
    /// among active teammates — caller handles generation/dedup.
    pub async fn insert(&self, view: TeammateView) {
        self.by_name.write().await.insert(view.name.clone(), view);
    }

    pub async fn mark_status(&self, name: &str, status: &str) {
        if let Some(v) = self.by_name.write().await.get_mut(name) {
            v.status = status.to_string();
            v.last_event_at = Utc::now();
        }
    }

    pub async fn touch(&self, name: &str) {
        if let Some(v) = self.by_name.write().await.get_mut(name) {
            v.last_event_at = Utc::now();
        }
    }

    pub async fn list(&self) -> Vec<TeammateView> {
        self.by_name.read().await.values().cloned().collect()
    }

    pub async fn get(&self, name: &str) -> Option<TeammateView> {
        self.by_name.read().await.get(name).cloned()
    }

    pub async fn get_by_pearl(&self, pearl_id: &str) -> Option<TeammateView> {
        self.by_name.read().await.values().find(|v| v.pearl_id == pearl_id).cloned()
    }
}

/// Poll the given pearl's comments and broadcast `TeammateChat` /
/// `TeammateIdle` events for newly-arrived teammate-originated traffic.
/// Designed to run for the lifetime of a single dispatch — exits when
/// the broadcast channel is closed (i.e., Big Smooth shutting down).
pub async fn spawn_comment_tap(
    pearl_store: smooth_pearls::PearlStore,
    pearl_id: String,
    teammate_name: String,
    event_tx: broadcast::Sender<ServerEvent>,
    registry: Arc<OperatorRegistry>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut ticker = tokio::time::interval(Duration::from_millis(1500));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut consecutive_errors = 0u32;
        let mut first = true;

        loop {
            ticker.tick().await;
            if event_tx.receiver_count() == 0 && !first {
                // No subscribers; keep running but don't waste cycles.
                // (Web UI may reconnect; we keep emitting.)
            }
            let comments = match pearl_store.get_comments(&pearl_id) {
                Ok(c) => {
                    consecutive_errors = 0;
                    c
                }
                Err(e) => {
                    consecutive_errors += 1;
                    if consecutive_errors > 20 {
                        tracing::warn!(error = %e, pearl_id = %pearl_id, "comment-tap: too many errors, exiting");
                        return;
                    }
                    continue;
                }
            };

            for c in comments {
                if !seen.insert(c.id.clone()) {
                    continue;
                }
                if first {
                    continue;
                }
                let trimmed = c.content.trim_start();
                let kind = if trimmed.starts_with("[CHAT:TEAMMATE]") {
                    Some(("chat", strip_prefix(trimmed, "[CHAT:TEAMMATE]")))
                } else if trimmed.starts_with("[PROGRESS]") {
                    Some(("progress", strip_prefix(trimmed, "[PROGRESS]")))
                } else if trimmed.starts_with("[QUESTION:TEAMMATE") {
                    Some(("question", strip_after_close_bracket(trimmed)))
                } else if trimmed.starts_with("[IDLE]") {
                    Some(("idle", String::new()))
                } else {
                    None
                };
                let Some((kind, body)) = kind else { continue };

                registry.touch(&teammate_name).await;
                let _ = event_tx.send(ServerEvent::TeammateChat {
                    teammate_name: teammate_name.clone(),
                    pearl_id: pearl_id.clone(),
                    kind: kind.to_string(),
                    message: body,
                    comment_id: c.id.clone(),
                });

                if kind == "idle" {
                    registry.mark_status(&teammate_name, "idle").await;
                    let _ = event_tx.send(ServerEvent::TeammateIdle {
                        teammate_name: teammate_name.clone(),
                        pearl_id: pearl_id.clone(),
                    });
                    return;
                }
            }
            first = false;
        }
    })
}

fn strip_prefix(s: &str, prefix: &str) -> String {
    s.strip_prefix(prefix).unwrap_or(s).trim().to_string()
}

fn strip_after_close_bracket(s: &str) -> String {
    s.find(']').map(|i| s[i + 1..].trim().to_string()).unwrap_or_default()
}

/// Generate a stable slug from a pearl id when we don't have a smarter
/// name source. e.g. `th-83c220` → `teammate-83c220`. The chat-agent's
/// `teammate.spawn` may pass a friendlier name (Phase 5 cast roles).
pub fn slug_from_pearl(pearl_id: &str) -> String {
    let suffix = pearl_id.strip_prefix("th-").unwrap_or(pearl_id);
    format!("teammate-{suffix}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn registry_insert_list_get() {
        let reg = OperatorRegistry::new();
        let v = TeammateView {
            name: "backend-refactorer".into(),
            pearl_id: "th-abc123".into(),
            title: "Refactor auth".into(),
            status: "running".into(),
            started_at: Utc::now(),
            last_event_at: Utc::now(),
        };
        reg.insert(v.clone()).await;
        let list = reg.list().await;
        assert_eq!(list.len(), 1);
        let got = reg.get("backend-refactorer").await.unwrap();
        assert_eq!(got.pearl_id, "th-abc123");
    }

    #[tokio::test]
    async fn registry_get_by_pearl() {
        let reg = OperatorRegistry::new();
        reg.insert(TeammateView {
            name: "backend-refactorer".into(),
            pearl_id: "th-abc123".into(),
            title: "x".into(),
            status: "running".into(),
            started_at: Utc::now(),
            last_event_at: Utc::now(),
        })
        .await;
        let got = reg.get_by_pearl("th-abc123").await.unwrap();
        assert_eq!(got.name, "backend-refactorer");
        assert!(reg.get_by_pearl("th-nope").await.is_none());
    }

    #[tokio::test]
    async fn registry_mark_status() {
        let reg = OperatorRegistry::new();
        reg.insert(TeammateView {
            name: "x".into(),
            pearl_id: "th-1".into(),
            title: "x".into(),
            status: "running".into(),
            started_at: Utc::now(),
            last_event_at: Utc::now(),
        })
        .await;
        reg.mark_status("x", "idle").await;
        assert_eq!(reg.get("x").await.unwrap().status, "idle");
    }

    #[test]
    fn slug_from_pearl_drops_th_prefix() {
        assert_eq!(slug_from_pearl("th-83c220"), "teammate-83c220");
        assert_eq!(slug_from_pearl("free-form"), "teammate-free-form");
    }
}

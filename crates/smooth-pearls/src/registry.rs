//! Global project registry at `~/.smooth/registry.json`.
//!
//! Tracks which repos have pearl stores (`.smooth/dolt/`), enabling
//! multi-project views and cross-repo pearl queries.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// A registered project with its pearl store location.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectEntry {
    /// Absolute path to the project root (parent of `.smooth/`).
    pub path: PathBuf,
    /// Human-readable name (derived from directory name or git remote).
    pub name: String,
    /// When this project was first registered.
    pub registered_at: DateTime<Utc>,
    /// When pearls were last accessed in this project.
    pub last_accessed: DateTime<Utc>,
}

/// The global registry of known pearl projects.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Registry {
    /// Map from project path (as string) to entry.
    pub projects: BTreeMap<String, ProjectEntry>,
}

impl Registry {
    /// Load the registry from `~/.smooth/registry.json`. Returns empty if not found.
    pub fn load() -> Result<Self> {
        let path = Self::registry_path()?;
        if !path.exists() {
            return Ok(Self::default());
        }
        let contents = std::fs::read_to_string(&path)?;
        let registry: Registry = serde_json::from_str(&contents)?;
        Ok(registry)
    }

    /// Save the registry to `~/.smooth/registry.json`.
    pub fn save(&self) -> Result<()> {
        let path = Self::registry_path()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(&path, json)?;
        Ok(())
    }

    /// Register a project. Updates `last_accessed` if already registered.
    pub fn register(&mut self, project_path: &Path, name: &str) {
        let key = project_path.to_string_lossy().to_string();
        let now = Utc::now();
        if let Some(entry) = self.projects.get_mut(&key) {
            entry.last_accessed = now;
            if entry.name != name {
                entry.name = name.to_string();
            }
        } else {
            self.projects.insert(
                key,
                ProjectEntry {
                    path: project_path.to_path_buf(),
                    name: name.to_string(),
                    registered_at: now,
                    last_accessed: now,
                },
            );
        }
    }

    /// Remove a project from the registry.
    pub fn unregister(&mut self, project_path: &Path) {
        let key = project_path.to_string_lossy().to_string();
        self.projects.remove(&key);
    }

    /// Touch `last_accessed` for a project.
    pub fn touch(&mut self, project_path: &Path) {
        let key = project_path.to_string_lossy().to_string();
        if let Some(entry) = self.projects.get_mut(&key) {
            entry.last_accessed = Utc::now();
        }
    }

    /// List all registered projects, sorted by last accessed (most recent first).
    pub fn list(&self) -> Vec<&ProjectEntry> {
        let mut entries: Vec<&ProjectEntry> = self.projects.values().collect();
        entries.sort_by(|a, b| b.last_accessed.cmp(&a.last_accessed));
        entries
    }

    /// Prune entries whose project paths no longer exist on disk.
    pub fn prune(&mut self) -> usize {
        let before = self.projects.len();
        self.projects.retain(|_, entry| entry.path.join(".smooth").join("dolt").exists());
        before - self.projects.len()
    }

    fn registry_path() -> Result<PathBuf> {
        let home = dirs_next::home_dir().ok_or_else(|| anyhow::anyhow!("cannot determine home directory"))?;
        Ok(home.join(".smooth").join("registry.json"))
    }
}

/// Auto-register the current project when opening a pearl store.
/// Call this from `PearlStore::open` or `th pearls init`.
pub fn auto_register(project_root: &Path) -> Result<()> {
    let name = project_root
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "unknown".to_string());
    let mut registry = Registry::load()?;
    registry.register(project_root, &name);
    registry.save()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_registry_register_and_list() {
        let mut reg = Registry::default();
        reg.register(Path::new("/tmp/project-a"), "project-a");
        reg.register(Path::new("/tmp/project-b"), "project-b");

        let list = reg.list();
        assert_eq!(list.len(), 2);
    }

    #[test]
    fn test_registry_unregister() {
        let mut reg = Registry::default();
        reg.register(Path::new("/tmp/project-a"), "project-a");
        reg.unregister(Path::new("/tmp/project-a"));
        assert!(reg.projects.is_empty());
    }

    #[test]
    fn test_registry_touch_updates_last_accessed() {
        let mut reg = Registry::default();
        reg.register(Path::new("/tmp/project-a"), "project-a");
        let first = reg.projects["/tmp/project-a"].last_accessed;
        std::thread::sleep(std::time::Duration::from_millis(10));
        reg.touch(Path::new("/tmp/project-a"));
        let second = reg.projects["/tmp/project-a"].last_accessed;
        assert!(second > first);
    }

    #[test]
    fn test_registry_serialization_roundtrip() {
        let mut reg = Registry::default();
        reg.register(Path::new("/tmp/project-a"), "project-a");

        let json = serde_json::to_string(&reg).unwrap();
        let deser: Registry = serde_json::from_str(&json).unwrap();
        assert_eq!(deser.projects.len(), 1);
        assert_eq!(deser.projects["/tmp/project-a"].name, "project-a");
    }
}

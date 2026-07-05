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
        entries.sort_by_key(|e| std::cmp::Reverse(e.last_accessed));
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
///
/// Serialized through both a process-wide mutex and a cross-process
/// OS file lock so concurrent `PearlStore::init` calls (including
/// nextest, which spawns one process per test) can't race the
/// load → modify → save sequence and lose entries — pearls
/// `th-96e525` (in-process) and `th-9799fa` (cross-process).
pub fn auto_register(project_root: &Path) -> Result<()> {
    let registry_path = Registry::registry_path()?;
    auto_register_at(project_root, &registry_path)
}

/// Same as [`auto_register`] but writes to an explicit registry file.
/// Exposed for tests that want to exercise the concurrency lock
/// without touching `~/.smooth/registry.json`.
pub fn auto_register_at(project_root: &Path, registry_path: &Path) -> Result<()> {
    use fs4::fs_std::FileExt;

    // In-process Mutex: fast path for thread-races inside the same
    // process (`cargo test` runs tests as threads in one binary).
    // The file lock below catches cross-process races (`cargo nextest`
    // runs each test in its own process — the in-process mutex is
    // useless there).
    static REGISTRY_WRITE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = REGISTRY_WRITE_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());

    if let Some(parent) = registry_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // Cross-process exclusive lock on a sidecar file. Using a sidecar
    // instead of locking the json directly so the lock acquisition
    // doesn't race the json open: open-with-create + lock would lose
    // the truncate, and locking BEFORE creating means the json may
    // not yet exist. The sidecar always exists once we create it
    // here.
    let lock_path = registry_path.with_extension("lock");
    let lock_file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)?;
    lock_file.lock_exclusive()?;
    // _lock_drop holds the lock until end of function. Drop releases
    // it (per fs4 docs).
    let _lock_drop = LockGuard(&lock_file);

    let name = project_root
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "unknown".to_string());

    let mut registry = if registry_path.exists() {
        let contents = std::fs::read_to_string(registry_path)?;
        serde_json::from_str(&contents)?
    } else {
        Registry::default()
    };
    registry.register(project_root, &name);
    let json = serde_json::to_string_pretty(&registry)?;
    std::fs::write(registry_path, json)?;
    Ok(())
}

/// RAII guard that releases the fs4 file lock on drop. Without this
/// the lock would only release when `lock_file` itself is dropped at
/// end of scope, which is the same effect — but having the guard
/// makes the intent obvious to readers and prevents accidental
/// reordering.
struct LockGuard<'a>(&'a std::fs::File);
impl Drop for LockGuard<'_> {
    fn drop(&mut self) {
        use fs4::fs_std::FileExt;
        let _ = FileExt::unlock(self.0);
    }
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

    /// Pearl `th-96e525`: prior to the process-wide mutex in
    /// `auto_register_at`, concurrent registrations would race the
    /// load → modify → save sequence and lose entries — flaking the
    /// bigsmooth `project_pearls_returns_pearls_for_path` integration
    /// test in CI. This test fans out N concurrent registrations
    /// against a single file and asserts all N survive.
    #[test]
    fn auto_register_at_serializes_concurrent_writers() {
        const WRITERS: usize = 16;
        let tmp = tempfile::tempdir().expect("tempdir");
        let registry_file = tmp.path().join("registry.json");

        let handles: Vec<_> = (0..WRITERS)
            .map(|i| {
                let registry_file = registry_file.clone();
                let project_root = tmp.path().join(format!("project-{i}"));
                std::thread::spawn(move || {
                    std::fs::create_dir_all(&project_root).expect("create project root");
                    auto_register_at(&project_root, &registry_file).expect("auto_register_at");
                })
            })
            .collect();

        for h in handles {
            h.join().expect("thread join");
        }

        let contents = std::fs::read_to_string(&registry_file).expect("read registry");
        let registry: Registry = serde_json::from_str(&contents).expect("parse registry");
        assert_eq!(registry.projects.len(), WRITERS, "all {WRITERS} concurrent registrations must survive");
    }

    /// Pearl `th-9799fa`: the in-process Mutex above doesn't help
    /// `cargo nextest`, which runs each test in its own process.
    /// This test fans out N concurrent OS processes (each invoking
    /// the `auto_register_cross_process_writer` example binary) and
    /// asserts all N entries survive — proving the fs4 file lock
    /// holds across process boundaries.
    #[test]
    fn auto_register_at_serializes_cross_process_writers() {
        const WRITERS: usize = 12;

        let helper = find_example_binary("auto_register_cross_process_writer");
        let Some(helper) = helper else {
            eprintln!("skipping cross-process test: example binary not built (run `cargo build --examples -p smooai-smooth-pearls`)");
            return;
        };

        let tmp = tempfile::tempdir().expect("tempdir");
        let registry_file = tmp.path().join("registry.json");

        let children: Vec<_> = (0..WRITERS)
            .map(|i| {
                let project_root = tmp.path().join(format!("xp-project-{i}"));
                std::process::Command::new(&helper)
                    .arg(&registry_file)
                    .arg(&project_root)
                    .spawn()
                    .expect("spawn writer process")
            })
            .collect();

        for mut child in children {
            let status = child.wait().expect("wait for writer process");
            assert!(status.success(), "writer process exited non-zero: {status:?}");
        }

        let contents = std::fs::read_to_string(&registry_file).expect("read registry");
        let registry: Registry = serde_json::from_str(&contents).expect("parse registry");
        assert_eq!(
            registry.projects.len(),
            WRITERS,
            "all {WRITERS} cross-process registrations must survive — file lock isn't holding across processes"
        );
    }

    /// Locate an example binary built alongside the test. Cargo puts
    /// examples in `<target>/<profile>/examples/<name>` — walk up
    /// from `current_exe()` (the test binary in `deps/`) to find it.
    /// Returns None if the example hasn't been built yet.
    fn find_example_binary(name: &str) -> Option<std::path::PathBuf> {
        let exe = std::env::current_exe().ok()?;
        // `<target>/<profile>/deps/<test>-<hash>` → up two = profile dir.
        let profile_dir = exe.parent()?.parent()?;
        let candidate = profile_dir.join("examples").join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
        // CARGO_TARGET_DIR override or alternate layout: try
        // <profile>/examples/<name>.exe on windows.
        #[cfg(windows)]
        {
            let win = profile_dir.join("examples").join(format!("{name}.exe"));
            if win.is_file() {
                return Some(win);
            }
        }
        None
    }
}

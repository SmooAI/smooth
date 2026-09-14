//! Global project registry at `~/.smooth/registry.json`.
//!
//! Tracks which projects have pearls in `~/.smooth/pearls.db`, enabling
//! multi-project views and cross-repo pearl queries.
//!
//! Opening a store from anywhere used to register whatever
//! `resolve_project_root` returned, and `th prime` (a SessionStart hook)
//! opens the store from any cwd — so the registry filled with Codex scratch
//! dirs, `$TMPDIR`, `$HOME`, even `/` (pearl th-92e046). Now an *implicit*
//! registration (a plain store open) only lands when the root is a git
//! repository and is neither `/` nor `$HOME`; `th pearls init` registers
//! *explicitly* and accepts any directory. Every registration also prunes
//! entries whose path is gone or, unless they were explicit, is not a git
//! repository — so existing litter heals on the next open.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// A registered project with its pearl store location.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectEntry {
    /// Absolute path to the project root (the `project` column in pearls.db).
    pub path: PathBuf,
    /// Human-readable name (derived from directory name or git remote).
    pub name: String,
    /// When this project was first registered.
    pub registered_at: DateTime<Utc>,
    /// When pearls were last accessed in this project.
    pub last_accessed: DateTime<Utc>,
    /// `true` when the project was registered on purpose (`th pearls init`)
    /// rather than as a side effect of opening the store. Explicit entries
    /// are exempt from the not-a-git-repo prune; missing from older
    /// registries, which reads as `false`.
    #[serde(default)]
    pub explicit: bool,
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
        Self::parse(&std::fs::read_to_string(&path)?)
    }

    /// Parse registry JSON, treating an empty or whitespace-only file as an
    /// empty registry rather than a hard error. A zero-byte `registry.json`
    /// is a real state on disk — an interrupted `save()` truncates before it
    /// writes — and failing to parse it made every `auto_register` a silent
    /// no-op, so no project ever re-registered itself. The next `save()`
    /// heals the file.
    fn parse(contents: &str) -> Result<Self> {
        if contents.trim().is_empty() {
            return Ok(Self::default());
        }
        Ok(serde_json::from_str(contents)?)
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

    /// Register a project implicitly (a store open). Updates `last_accessed`
    /// if already registered. Does NOT check [`is_auto_registrable`] — that
    /// gate lives in [`auto_register_at`], so callers that already decided
    /// can insert directly.
    pub fn register(&mut self, project_path: &Path, name: &str) {
        self.register_with(project_path, name, false);
    }

    /// Register a project explicitly (`th pearls init`). An explicit entry
    /// survives the non-git prune, and re-registering an existing entry
    /// explicitly upgrades it; an implicit re-registration never downgrades.
    pub fn register_explicit(&mut self, project_path: &Path, name: &str) {
        self.register_with(project_path, name, true);
    }

    fn register_with(&mut self, project_path: &Path, name: &str, explicit: bool) {
        let key = project_path.to_string_lossy().to_string();
        let now = Utc::now();
        if let Some(entry) = self.projects.get_mut(&key) {
            entry.last_accessed = now;
            entry.explicit |= explicit;
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
                    explicit,
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

    /// Prune entries whose project path no longer exists on disk, plus
    /// implicit entries that would not auto-register today (not a git
    /// repository, or `/` / `$HOME`). Explicit entries only go when their
    /// directory does. Returns how many were dropped.
    pub fn prune(&mut self) -> usize {
        self.prune_with_home(dirs_next::home_dir().as_deref())
    }

    fn prune_with_home(&mut self, home: Option<&Path>) -> usize {
        let before = self.projects.len();
        self.projects
            .retain(|_, entry| entry.path.exists() && (entry.explicit || is_auto_registrable_with(&entry.path, home)));
        before - self.projects.len()
    }

    fn registry_path() -> Result<PathBuf> {
        let home = dirs_next::home_dir().ok_or_else(|| anyhow::anyhow!("cannot determine home directory"))?;
        Ok(home.join(".smooth").join("registry.json"))
    }
}

/// Whether a plain store open may register `root` on its own.
///
/// `root` must be a git repository (a `.git` entry at the root — the store
/// already collapsed linked worktrees to the main checkout, so a directory,
/// but a file is accepted too) and must not be the filesystem root or
/// `$HOME`. A git repository is the only signal that a directory is a
/// *project* rather than wherever a hook happened to run; `/` and `$HOME`
/// are excluded even when someone has `git init`-ed their home.
///
/// This is a filesystem check rather than `git rev-parse` so the prune
/// that runs on every store open costs no subprocess per entry.
#[must_use]
pub fn is_auto_registrable(root: &Path) -> bool {
    is_auto_registrable_with(root, dirs_next::home_dir().as_deref())
}

fn is_auto_registrable_with(root: &Path, home: Option<&Path>) -> bool {
    if root.parent().is_none() {
        return false;
    }
    if home.is_some_and(|h| h == root) {
        return false;
    }
    root.join(".git").exists()
}

/// Register the project implicitly when opening a pearl store (called from
/// `PearlStore::open`). A no-op — apart from the prune — when `project_root`
/// fails [`is_auto_registrable`].
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

/// Register the project explicitly (`th pearls init`): any directory is
/// accepted and the entry is marked `explicit`, so it survives the
/// non-git prune.
///
/// # Errors
/// When `~/.smooth` cannot be resolved, or the registry file cannot be
/// locked, read, or written.
pub fn register_explicit(project_root: &Path) -> Result<()> {
    let registry_path = Registry::registry_path()?;
    register_explicit_at(project_root, &registry_path)
}

/// Same as [`auto_register`] but writes to an explicit registry file.
/// Exposed for tests that want to exercise the concurrency lock
/// without touching `~/.smooth/registry.json`.
pub fn auto_register_at(project_root: &Path, registry_path: &Path) -> Result<()> {
    register_at(project_root, registry_path, false)
}

/// Same as [`register_explicit`] but writes to an explicit registry file.
///
/// # Errors
/// When the registry file cannot be locked, read, or written.
pub fn register_explicit_at(project_root: &Path, registry_path: &Path) -> Result<()> {
    register_at(project_root, registry_path, true)
}

fn register_at(project_root: &Path, registry_path: &Path, explicit: bool) -> Result<()> {
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
        Registry::parse(&std::fs::read_to_string(registry_path)?)?
    } else {
        Registry::default()
    };
    if explicit {
        registry.register_explicit(project_root, &name);
    } else if is_auto_registrable(project_root) {
        registry.register(project_root, &name);
    }
    // Drop entries whose directory is gone (deleted worktrees, old tempdirs)
    // and implicit ones that are not git repositories (hook litter). This
    // runs even when nothing was registered, so any open heals the file.
    registry.prune();
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

    /// A directory that passes [`is_auto_registrable`]: a `.git` entry is
    /// all the gate looks for, so no real `git init` is needed.
    fn mk_repo(path: &Path) {
        std::fs::create_dir_all(path.join(".git")).expect("create fake repo");
    }

    fn parse_file(path: &Path) -> Registry {
        Registry::parse(&std::fs::read_to_string(path).expect("read registry")).expect("parse registry")
    }

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

    /// Pearl `th-91de11`: a zero-byte `~/.smooth/registry.json` (left by an
    /// interrupted `save()`) used to fail JSON parsing, so `Registry::load`
    /// and every `auto_register` errored out and no project re-registered.
    /// An empty or whitespace-only file must read as an empty registry.
    #[test]
    fn parse_treats_empty_file_as_empty_registry() {
        for blank in ["", "   ", "\n\t \n"] {
            let reg = Registry::parse(blank).expect("blank registry.json must parse as empty");
            assert!(reg.projects.is_empty());
        }
        assert!(Registry::parse("{ not json").is_err(), "genuinely corrupt JSON must still error");
    }

    /// The healing half: `auto_register_at` against a zero-byte registry
    /// must register the project and leave valid JSON behind.
    #[test]
    fn auto_register_heals_a_zero_byte_registry() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let registry_file = tmp.path().join("registry.json");
        std::fs::write(&registry_file, "").expect("write empty registry");

        let project_root = tmp.path().join("proj");
        mk_repo(&project_root);
        auto_register_at(&project_root, &registry_file).expect("auto_register_at must heal an empty file");

        let contents = std::fs::read_to_string(&registry_file).expect("read registry");
        let registry: Registry = serde_json::from_str(&contents).expect("healed registry must be valid JSON");
        assert_eq!(registry.projects.len(), 1);
    }

    #[test]
    fn auto_register_prunes_dead_paths() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let registry_file = tmp.path().join("registry.json");
        let gone = tmp.path().join("gone");
        mk_repo(&gone);
        auto_register_at(&gone, &registry_file).unwrap();
        std::fs::remove_dir_all(&gone).unwrap();
        let alive = tmp.path().join("alive");
        mk_repo(&alive);
        auto_register_at(&alive, &registry_file).unwrap();
        let registry = Registry::parse(&std::fs::read_to_string(&registry_file).unwrap()).unwrap();
        assert_eq!(registry.projects.len(), 1);
        assert!(registry.projects.values().all(|e| e.path == alive));
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
                    mk_repo(&project_root);
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

    /// Pearl `th-92e046`: `th prime` opens the store from any cwd, so a
    /// plain open must NOT register a directory that is not a git repo —
    /// but it must still leave a valid (empty) registry behind.
    #[test]
    fn auto_register_skips_non_git_dir() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let registry_file = tmp.path().join("registry.json");
        let scratch = tmp.path().join("Codex").join("2026-09-08").join("is-x20");
        std::fs::create_dir_all(&scratch).unwrap();
        auto_register_at(&scratch, &registry_file).unwrap();
        let registry = parse_file(&registry_file);
        assert!(
            registry.projects.is_empty(),
            "a non-git dir must not auto-register: {:?}",
            registry.projects.keys()
        );
    }

    #[test]
    fn auto_register_accepts_git_dir_including_worktree_gitfile() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let registry_file = tmp.path().join("registry.json");
        let main = tmp.path().join("main");
        mk_repo(&main);
        let linked = tmp.path().join("linked");
        std::fs::create_dir_all(&linked).unwrap();
        std::fs::write(linked.join(".git"), "gitdir: ../main/.git/worktrees/linked\n").unwrap();
        auto_register_at(&main, &registry_file).unwrap();
        auto_register_at(&linked, &registry_file).unwrap();
        let registry = parse_file(&registry_file);
        assert_eq!(registry.projects.len(), 2);
        assert!(registry.projects.values().all(|e| !e.explicit));
    }

    #[test]
    fn is_auto_registrable_rejects_filesystem_root_and_home() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path().join("home");
        mk_repo(&home); // a `git init`-ed $HOME is still not a project
        let root = Path::new("/");
        assert!(!is_auto_registrable_with(root, Some(&home)));
        assert!(!is_auto_registrable_with(&home, Some(&home)));
        let project = home.join("dev").join("proj");
        mk_repo(&project);
        assert!(is_auto_registrable_with(&project, Some(&home)));
        assert!(!is_auto_registrable_with(&home.join("dev"), Some(&home)), "a plain parent dir is not a project");
    }

    /// `th pearls init` is the explicit opt-in: any directory registers,
    /// the entry is marked, and it survives every prune while it exists.
    #[test]
    fn explicit_register_accepts_non_git_dir_and_survives_prune() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let registry_file = tmp.path().join("registry.json");
        let notes = tmp.path().join("notes");
        std::fs::create_dir_all(&notes).unwrap();
        register_explicit_at(&notes, &registry_file).unwrap();
        let registry = parse_file(&registry_file);
        assert_eq!(registry.projects.len(), 1);
        assert!(registry.projects.values().all(|e| e.explicit && e.path == notes));

        // A later implicit open elsewhere prunes litter but keeps it.
        let scratch = tmp.path().join("scratch");
        std::fs::create_dir_all(&scratch).unwrap();
        auto_register_at(&scratch, &registry_file).unwrap();
        let registry = parse_file(&registry_file);
        assert_eq!(registry.projects.len(), 1);
        assert!(registry.projects.values().all(|e| e.path == notes));
    }

    #[test]
    fn explicit_flag_is_sticky_across_implicit_reregistration() {
        let mut reg = Registry::default();
        reg.register_explicit(Path::new("/tmp/project-a"), "project-a");
        reg.register(Path::new("/tmp/project-a"), "project-a");
        assert!(reg.projects["/tmp/project-a"].explicit, "implicit re-register must not downgrade");
        reg.register(Path::new("/tmp/project-b"), "project-b");
        reg.register_explicit(Path::new("/tmp/project-b"), "project-b");
        assert!(reg.projects["/tmp/project-b"].explicit, "explicit re-register must upgrade");
    }

    /// Registries written before the flag existed carry no `explicit`
    /// field; they must read as implicit, so the litter they hold prunes.
    #[test]
    fn legacy_registry_json_reads_explicit_as_false() {
        let json = r#"{"projects":{"/tmp/legacy":{"path":"/tmp/legacy","name":"legacy","registered_at":"2026-01-01T00:00:00Z","last_accessed":"2026-01-01T00:00:00Z"}}}"#;
        let reg = Registry::parse(json).unwrap();
        assert!(!reg.projects["/tmp/legacy"].explicit);
    }

    /// The healing half of th-92e046: an existing registry full of hook
    /// litter (scratch dirs, a tmpdir, `$HOME`, `/`) is cleaned by the
    /// next open — even one that registers nothing itself — while git
    /// repos and explicit entries stay.
    #[test]
    fn prune_drops_non_git_litter_but_keeps_repos_and_explicit_entries() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path().join("home");
        mk_repo(&home);
        let repo = home.join("dev").join("repo");
        mk_repo(&repo);
        let scratch = home.join("Documents").join("Codex").join("x");
        std::fs::create_dir_all(&scratch).unwrap();
        let explicit_notes = home.join("notes");
        std::fs::create_dir_all(&explicit_notes).unwrap();
        let gone = home.join("gone");

        let mut reg = Registry::default();
        reg.register(Path::new("/"), "unknown");
        reg.register(&home, "home");
        reg.register(&repo, "repo");
        reg.register(&scratch, "x");
        reg.register(&gone, "gone");
        reg.register_explicit(&explicit_notes, "notes");
        assert_eq!(reg.projects.len(), 6);

        let dropped = reg.prune_with_home(Some(&home));
        assert_eq!(dropped, 4, "/, $HOME, the scratch dir and the missing dir must go");
        let mut kept: Vec<&Path> = reg.projects.values().map(|e| e.path.as_path()).collect();
        kept.sort();
        let mut expected = vec![repo.as_path(), explicit_notes.as_path()];
        expected.sort();
        assert_eq!(kept, expected);
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

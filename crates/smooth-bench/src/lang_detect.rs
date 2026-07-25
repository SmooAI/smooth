//! Workspace language + test-command detection for `score-replay`.
//!
//! The existing aider-polyglot path can hard-code the language (the
//! dataset directory tells us). When we replay a real-world PR we
//! don't have that luxury — the harvested repo could be any of
//! Cargo, Pytest, Npm/Jest, or a polyglot mix (e.g. a TS monorepo
//! that ships a Python tool alongside it). This module makes a single
//! cheap pass over the workspace root and reports what it found.
//!
//! Detection is **filesystem-only** — we never execute build tools.
//! That keeps the function side-effect-free and safe to call before
//! we've decided whether the workdir is trusted.
//!
//! The picker stays here (`test_command`) so the same module owns
//! both "what is this?" and "how do I test it?" — when we add a new
//! language we touch one file.

use std::path::{Path, PathBuf};

/// What we detected at the root of a workspace.
///
/// `Mixed` means we found markers for more than one language at the
/// same root. Callers can choose to run every detected test command
/// (default) or pick the first one — see [`test_command`] for the
/// fan-out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Workspace {
    Cargo,
    Pytest,
    Npm,
    Mixed(Vec<Self>),
    Unknown,
}

/// Inspect `root` for build-system markers and report what we find.
///
/// Pure filesystem; no subprocesses. Looks one level deep — markers
/// nested inside a subdirectory of a polyglot repo are not surfaced
/// here. That's deliberate: the caller of `score-replay` checks out
/// the repo at the PR's base SHA and runs the test command from the
/// root, so the root-level marker is what governs the canonical test
/// command.
#[must_use]
pub fn detect(root: &Path) -> Workspace {
    let mut found = Vec::new();
    if root.join("Cargo.toml").is_file() {
        found.push(Workspace::Cargo);
    }
    if root.join("pyproject.toml").is_file() || root.join("setup.py").is_file() || root.join("pytest.ini").is_file() || root.join("tox.ini").is_file() {
        found.push(Workspace::Pytest);
    }
    if root.join("package.json").is_file() {
        found.push(Workspace::Npm);
    }
    if found.is_empty() {
        return Workspace::Unknown;
    }
    if found.len() == 1 {
        // Single entry — unwrap the Vec without a panicking expect.
        // We just length-checked, so `into_iter().next()` always yields.
        return found.into_iter().next().unwrap_or(Workspace::Unknown);
    }
    Workspace::Mixed(found)
}

/// Resolve the test command(s) to run for `ws`.
///
/// Returns a `Vec<Vec<String>>` — each inner vector is one argv. The
/// outer vector lets `Mixed` repos return multiple commands the
/// caller runs in sequence; `Unknown` returns an empty vector so
/// callers can detect "nothing to do" without an Option dance.
///
/// `focused_files` is the set of source files the human PR touched.
/// For most workspaces we just run the full test suite (passing
/// focused paths to e.g. `pytest`/`jest` is technically possible but
/// brittle — many repos require running the suite as a unit for
/// fixtures, conftest.py discovery, etc.). The parameter is reserved
/// for future per-file targeting (pearl follow-up); for v1 we ignore
/// it. Surfacing it in the signature now keeps the call sites stable
/// when we do plumb it through.
#[must_use]
pub fn test_command(ws: &Workspace, focused_files: &[PathBuf]) -> Vec<Vec<String>> {
    // `focused_files` is reserved for future per-file targeting; v1
    // ignores it but accepts it in the signature so the call sites
    // remain stable when we plumb it through. The `_ = …` discard is
    // there to keep the parameter live without tripping clippy's
    // `only_used_in_recursion` (we genuinely just don't use it today).
    let _ = focused_files;
    match ws {
        Workspace::Cargo => vec![vec!["cargo".into(), "test".into(), "--".into(), "--include-ignored".into()]],
        Workspace::Pytest => vec![vec!["python3".into(), "-m".into(), "pytest".into(), "-q".into()]],
        Workspace::Npm => vec![vec!["sh".into(), "-c".into(), "npm install --silent --no-audit --no-fund && npm test".into()]],
        Workspace::Mixed(inner) => {
            // Fan out to every detected sub-workspace, preserving
            // detection order. Skips Unknown / nested Mixed (we
            // only ever construct one level deep in `detect`, but
            // the recursive shape keeps the function total). We
            // pass an empty `focused_files` slice in the recursive
            // call — recursion would otherwise be the only use, and
            // clippy flags that as a smell.
            let mut out = Vec::new();
            for w in inner {
                out.extend(test_command(w, &[]));
            }
            out
        }
        Workspace::Unknown => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn cargo_root_detects_cargo() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "[package]\nname=\"x\"\nversion=\"0.1.0\"\nedition=\"2021\"\n").unwrap();
        assert_eq!(detect(dir.path()), Workspace::Cargo);
    }

    #[test]
    fn pyproject_detects_pytest() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("pyproject.toml"), "[project]\nname='x'\n").unwrap();
        assert_eq!(detect(dir.path()), Workspace::Pytest);
    }

    #[test]
    fn setup_py_detects_pytest() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("setup.py"), "from setuptools import setup\nsetup(name='x')\n").unwrap();
        assert_eq!(detect(dir.path()), Workspace::Pytest);
    }

    #[test]
    fn package_json_detects_npm() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("package.json"), "{\"name\":\"x\"}").unwrap();
        assert_eq!(detect(dir.path()), Workspace::Npm);
    }

    #[test]
    fn empty_root_returns_unknown() {
        let dir = tempdir().unwrap();
        assert_eq!(detect(dir.path()), Workspace::Unknown);
    }

    #[test]
    fn polyglot_returns_mixed() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "[package]\nname=\"x\"\nversion=\"0.1.0\"\nedition=\"2021\"\n").unwrap();
        std::fs::write(dir.path().join("pyproject.toml"), "[project]\nname='x'\n").unwrap();
        let detected = detect(dir.path());
        match detected {
            Workspace::Mixed(inner) => {
                assert!(inner.contains(&Workspace::Cargo));
                assert!(inner.contains(&Workspace::Pytest));
            }
            other => panic!("expected Mixed, got {other:?}"),
        }
    }

    #[test]
    fn test_command_for_cargo_uses_include_ignored() {
        let argv = test_command(&Workspace::Cargo, &[]);
        assert_eq!(argv.len(), 1);
        assert_eq!(argv[0][0], "cargo");
        assert!(argv[0].contains(&"--include-ignored".to_string()));
    }

    #[test]
    fn test_command_for_pytest_uses_quiet() {
        let argv = test_command(&Workspace::Pytest, &[]);
        assert_eq!(argv.len(), 1);
        assert_eq!(argv[0][0], "python3");
        assert!(argv[0].contains(&"-q".to_string()));
    }

    #[test]
    fn test_command_for_npm_installs_first() {
        let argv = test_command(&Workspace::Npm, &[]);
        assert_eq!(argv.len(), 1);
        assert!(argv[0].iter().any(|s| s.contains("npm install")));
        assert!(argv[0].iter().any(|s| s.contains("npm test")));
    }

    #[test]
    fn test_command_for_unknown_is_empty() {
        let argv = test_command(&Workspace::Unknown, &[]);
        assert!(argv.is_empty());
    }

    #[test]
    fn test_command_for_mixed_fans_out() {
        let mixed = Workspace::Mixed(vec![Workspace::Cargo, Workspace::Pytest]);
        let argv = test_command(&mixed, &[]);
        assert_eq!(argv.len(), 2);
        assert_eq!(argv[0][0], "cargo");
        assert_eq!(argv[1][0], "python3");
    }

    #[test]
    fn test_command_ignores_focused_files_in_v1() {
        // Documenting the current behavior: focused_files is reserved
        // but not yet wired. If we change this, update the doc on
        // `test_command` accordingly.
        let with_files = test_command(&Workspace::Pytest, &[PathBuf::from("tests/test_foo.py")]);
        let without_files = test_command(&Workspace::Pytest, &[]);
        assert_eq!(with_files, without_files);
    }
}

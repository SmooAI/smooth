//! Objective checks that RUN something after the agent's turn
//! (SMOODEV-4125).
//!
//! The substring checks in [`crate::agentic`] answer "did it write the
//! current idiom?". They cannot answer "does it build?", "do the tests
//! pass?" or "did it touch files it had no business touching?" — and
//! those are the questions that separate an agent you can hand a real
//! change from one that produces plausible-looking text. This module adds
//! them:
//!
//! - [`CommandCheck`] — run a shell command in the finished workspace
//!   (`npm install`, `npm test`, `node --test`) and require exit 0.
//! - [`ServeCheck`] — start the app, wait for a URL to answer, check the
//!   body, tear it down. The headless smoke test.
//! - [`ScopeRule`] — diff the workspace against what was seeded and
//!   require every change to fall inside an allow-list.
//! - Held-out tests: a fixture directory copied in AFTER the turn,
//!   overwriting anything the agent wrote at those paths, so the agent can
//!   neither see nor weaken them.
//!
//! These run on the HOST, in the scenario's scratch workspace, with the
//! bench's own environment. That is the same trust boundary as
//! `--isolation host`, where the agent's own `bash` already runs there.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::agentic::AssertResult;

/// Directories that are tooling output or dependency installs, never the
/// agent's authored work. Skipped by the scope diff, the judge's workspace
/// dump and the substring scan — a bundled `dist/` of React contains
/// `forwardRef` and would fail a stale-idiom check the agent never wrote.
pub const SKIP_DIRS: &[&str] = &["node_modules", ".git", "dist", ".next", "coverage", "target", ".smooth", ".vite", ".turbo"];

/// Whether `name` is a directory the workspace walkers skip.
#[must_use]
pub fn is_skipped_dir(name: &str) -> bool {
    SKIP_DIRS.contains(&name)
}

fn default_command_timeout() -> u64 {
    300
}

fn default_serve_timeout() -> u64 {
    60
}

/// A shell command that must exit 0 in the finished workspace.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CommandCheck {
    /// Run via `sh -c` with the workspace as cwd and `CI=1` set, so test
    /// runners take their one-shot path instead of watching.
    pub run: String,
    /// Wall-clock cap. A hung command is a failure, not a stalled suite.
    #[serde(default = "default_command_timeout")]
    pub timeout_s: u64,
    /// What a failure means, shown verbatim.
    pub reason: String,
}

/// Boot the app and require `url` to answer 2xx (optionally containing
/// `contains`) within `timeout_s`.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ServeCheck {
    pub run: String,
    pub url: String,
    #[serde(default)]
    pub contains: Option<String>,
    #[serde(default = "default_serve_timeout")]
    pub timeout_s: u64,
    pub reason: String,
}

/// Which seeded-repo paths the agent may add, change or delete.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ScopeRule {
    /// Glob patterns (`*` within a segment, `**` across segments).
    pub allow: Vec<String>,
    /// Upper bound on files touched, as a backstop against a rewrite that
    /// stays inside the allowed directories.
    #[serde(default)]
    pub max_changed_files: Option<usize>,
}

/// One file-level change relative to the seeded workspace.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Change {
    Added(String),
    Modified(String),
    Deleted(String),
}

impl Change {
    #[must_use]
    pub fn path(&self) -> &str {
        match self {
            Self::Added(p) | Self::Modified(p) | Self::Deleted(p) => p,
        }
    }

    #[must_use]
    pub fn render(&self) -> String {
        match self {
            Self::Added(p) => format!("A {p}"),
            Self::Modified(p) => format!("M {p}"),
            Self::Deleted(p) => format!("D {p}"),
        }
    }
}

/// Every file under `root` (workspace-relative, `/`-separated), skipping
/// [`SKIP_DIRS`].
#[must_use]
pub fn list_files(root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for e in entries.flatten() {
            let p = e.path();
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_dir() {
                if !p.file_name().and_then(|n| n.to_str()).is_some_and(is_skipped_dir) {
                    stack.push(p);
                }
            } else if ft.is_file() {
                if let Ok(rel) = p.strip_prefix(root) {
                    out.push(rel.components().map(|c| c.as_os_str().to_string_lossy()).collect::<Vec<_>>().join("/"));
                }
            }
        }
    }
    out.sort();
    out
}

/// Diff the workspace against what was seeded. Sorted by path.
#[must_use]
pub fn workspace_changes(work: &Path, seeded: &BTreeMap<String, String>) -> Vec<Change> {
    let mut out = Vec::new();
    let now = list_files(work);
    for rel in &now {
        match seeded.get(rel) {
            None => out.push(Change::Added(rel.clone())),
            Some(before) => {
                let after = std::fs::read(work.join(rel))
                    .map(|b| String::from_utf8_lossy(&b).into_owned())
                    .unwrap_or_default();
                if &after != before {
                    out.push(Change::Modified(rel.clone()));
                }
            }
        }
    }
    for rel in seeded.keys() {
        if !now.contains(rel) && !rel.split('/').any(is_skipped_dir) {
            out.push(Change::Deleted(rel.clone()));
        }
    }
    out.sort_by(|a, b| a.path().cmp(b.path()));
    out
}

/// Check `changes` against `rule`. One result per violation class, so a
/// pass reads as one line and a failure names every offending path.
#[must_use]
pub fn evaluate_scope(rule: &ScopeRule, changes: &[Change]) -> Vec<AssertResult> {
    let mut out = Vec::new();
    let outside: Vec<String> = changes
        .iter()
        .filter(|c| !rule.allow.iter().any(|g| glob_match(g, c.path())))
        .map(Change::render)
        .collect();
    out.push(if outside.is_empty() {
        AssertResult {
            ok: true,
            detail: format!("scope: all {} change(s) inside the allowed paths", changes.len()),
        }
    } else {
        AssertResult {
            ok: false,
            detail: format!("scope: touched files outside {:?}: {}", rule.allow, outside.join(", ")),
        }
    });
    if let Some(max) = rule.max_changed_files {
        out.push(AssertResult {
            ok: changes.len() <= max,
            detail: format!("scope: {} file(s) changed (max {max})", changes.len()),
        });
    }
    out
}

/// Minimal glob: `**` matches any number of path segments (including
/// none), `*` matches within one segment, everything else is literal.
#[must_use]
pub fn glob_match(pattern: &str, path: &str) -> bool {
    fn segs(pat: &[&str], path: &[&str]) -> bool {
        match pat.split_first() {
            None => path.is_empty(),
            Some((&"**", rest)) => (0..=path.len()).any(|i| segs(rest, &path[i..])),
            Some((p, rest)) => path
                .split_first()
                .is_some_and(|(s, prest)| seg_match(p.as_bytes(), s.as_bytes()) && segs(rest, prest)),
        }
    }
    fn seg_match(p: &[u8], s: &[u8]) -> bool {
        match p.split_first() {
            None => s.is_empty(),
            Some((b'*', rest)) => (0..=s.len()).any(|i| seg_match(rest, &s[i..])),
            Some((c, rest)) => s.split_first().is_some_and(|(d, srest)| c == d && seg_match(rest, srest)),
        }
    }
    let p: Vec<&str> = pattern.split('/').collect();
    let s: Vec<&str> = path.split('/').collect();
    segs(&p, &s)
}

/// Recursively copy `src` into `dst`, overwriting. Returns the
/// workspace-relative paths written (text content alongside, for the
/// seeded snapshot).
///
/// # Errors
/// Errors when `src` is missing or a file cannot be copied.
pub fn copy_tree(src: &Path, dst: &Path) -> Result<Vec<(String, String)>> {
    anyhow::ensure!(src.is_dir(), "fixture directory {} does not exist", src.display());
    let mut written = Vec::new();
    for rel in list_files(src) {
        let from = src.join(&rel);
        let to = dst.join(&rel);
        if let Some(parent) = to.parent() {
            std::fs::create_dir_all(parent).with_context(|| format!("mkdir {}", parent.display()))?;
        }
        std::fs::copy(&from, &to).with_context(|| format!("copying {} -> {}", from.display(), to.display()))?;
        let text = std::fs::read(&from).map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default();
        written.push((rel, text));
    }
    Ok(written)
}

/// Keep the END of a long output: that is where test runners put the
/// failure summary and the exit reason.
#[must_use]
pub fn tail(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n <= max {
        return s.to_string();
    }
    let t: String = s.chars().skip(n - max).collect();
    format!("…{t}")
}

/// Kill a whole process group. Package managers and dev servers fork
/// children that outlive the `sh` we spawned; killing only `sh` leaves a
/// server holding the port for the next scenario.
fn kill_group(pid: Option<u32>) {
    // `process_group(0)` is unix-only, so there is no group to kill elsewhere.
    #[cfg(unix)]
    if let Some(pid) = pid {
        let _ = std::process::Command::new("kill").arg("-KILL").arg(format!("-{pid}")).status();
    }
    #[cfg(not(unix))]
    let _ = pid;
}

fn shell(run: &str, cwd: &Path) -> tokio::process::Command {
    let mut c = tokio::process::Command::new("sh");
    c.arg("-c")
        .arg(run)
        .current_dir(cwd)
        .env("CI", "1")
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true);
    #[cfg(unix)]
    c.process_group(0);
    c
}

/// Run one [`CommandCheck`]. Never errors — a spawn failure is a failed
/// check with the reason attached.
pub async fn run_command(check: &CommandCheck, work: &Path) -> AssertResult {
    let mut cmd = shell(&check.run, work);
    cmd.stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped());
    let child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            return AssertResult {
                ok: false,
                detail: format!("`{}` could not start: {e} — {}", check.run, check.reason),
            }
        }
    };
    let pid = child.id();
    let res = tokio::time::timeout(Duration::from_secs(check.timeout_s), child.wait_with_output()).await;
    kill_group(pid);
    match res {
        Err(_) => AssertResult {
            ok: false,
            detail: format!("`{}` timed out after {}s — {}", check.run, check.timeout_s, check.reason),
        },
        Ok(Err(e)) => AssertResult {
            ok: false,
            detail: format!("`{}` failed to run: {e} — {}", check.run, check.reason),
        },
        Ok(Ok(out)) if out.status.success() => AssertResult {
            ok: true,
            detail: format!("`{}` exited 0", check.run),
        },
        Ok(Ok(out)) => {
            let mut log = String::from_utf8_lossy(&out.stdout).into_owned();
            log.push_str(&String::from_utf8_lossy(&out.stderr));
            AssertResult {
                ok: false,
                detail: format!(
                    "`{}` exited {} — {}\n{}",
                    check.run,
                    out.status.code().map_or_else(|| "by signal".to_string(), |c| c.to_string()),
                    check.reason,
                    tail(log.trim(), 1_500)
                ),
            }
        }
    }
}

/// Run one [`ServeCheck`]: boot, poll, tear down. Never errors.
pub async fn run_serve(check: &ServeCheck, work: &Path, log: Option<PathBuf>) -> AssertResult {
    let mut cmd = shell(&check.run, work);
    match log.as_deref().and_then(|p| std::fs::File::create(p).ok()) {
        Some(f) => match f.try_clone() {
            Ok(f2) => {
                cmd.stdout(f).stderr(f2);
            }
            Err(_) => {
                cmd.stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
            }
        },
        None => {
            cmd.stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
        }
    }
    let child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            return AssertResult {
                ok: false,
                detail: format!("serve `{}` could not start: {e} — {}", check.run, check.reason),
            }
        }
    };
    let pid = child.id();
    let client = reqwest::Client::builder().timeout(Duration::from_secs(5)).build().unwrap_or_default();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(check.timeout_s);
    let mut last = "never answered".to_string();
    let mut result = None;
    while tokio::time::Instant::now() < deadline {
        match client.get(&check.url).send().await {
            Ok(resp) => {
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                if status.is_success() && check.contains.as_deref().is_none_or(|c| body.contains(c)) {
                    result = Some(AssertResult {
                        ok: true,
                        detail: format!("serve: {} answered {status}", check.url),
                    });
                    break;
                }
                last = format!(
                    "answered {status}{}",
                    check.contains.as_deref().map_or(String::new(), |c| format!(" without {c:?}"))
                );
            }
            Err(e) => last = format!("unreachable ({e})"),
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    drop(child);
    kill_group(pid);
    result.unwrap_or_else(|| AssertResult {
        ok: false,
        detail: format!("serve: {} {last} within {}s — {}", check.url, check.timeout_s, check.reason),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "unwrap is the idiom for test assertions")]
mod tests {
    use super::*;

    #[test]
    fn glob_handles_literals_stars_and_globstars() {
        assert!(glob_match("src/cart.js", "src/cart.js"));
        assert!(!glob_match("src/cart.js", "src/cart.ts"));
        assert!(glob_match("src/*.js", "src/cart.js"));
        assert!(!glob_match("src/*.js", "src/lib/cart.js"), "* must not cross a segment");
        assert!(glob_match("src/**", "src/lib/deep/cart.js"));
        assert!(glob_match("**/*.test.js", "test/cart.test.js"));
        assert!(glob_match("**/*.test.js", "cart.test.js"), "** matches zero segments");
        assert!(glob_match("test/**/*.js", "test/a.js"));
        assert!(!glob_match("test/**", "src/a.js"));
        assert!(glob_match("*", "README.md"));
        assert!(!glob_match("*", "src/a.js"));
    }

    fn seeded(files: &[(&str, &str)]) -> BTreeMap<String, String> {
        files.iter().map(|(p, c)| ((*p).to_string(), (*c).to_string())).collect()
    }

    #[test]
    fn diff_reports_added_modified_deleted_and_skips_tooling_dirs() {
        let d = tempfile::tempdir().unwrap();
        let w = d.path();
        std::fs::create_dir_all(w.join("src")).unwrap();
        std::fs::create_dir_all(w.join("node_modules/x")).unwrap();
        std::fs::write(w.join("src/a.js"), "changed").unwrap();
        std::fs::write(w.join("src/b.js"), "same").unwrap();
        std::fs::write(w.join("src/new.js"), "n").unwrap();
        std::fs::write(w.join("node_modules/x/i.js"), "dep").unwrap();
        let before = seeded(&[("src/a.js", "orig"), ("src/b.js", "same"), ("src/gone.js", "g")]);
        let ch = workspace_changes(w, &before);
        assert_eq!(
            ch,
            vec![
                Change::Modified("src/a.js".into()),
                Change::Deleted("src/gone.js".into()),
                Change::Added("src/new.js".into()),
            ]
        );
    }

    #[test]
    fn scope_flags_out_of_bounds_and_overcount() {
        let rule = ScopeRule {
            allow: vec!["src/cart.js".into(), "test/**".into()],
            max_changed_files: Some(2),
        };
        let ok = evaluate_scope(&rule, &[Change::Modified("src/cart.js".into()), Change::Added("test/x.test.js".into())]);
        assert!(ok.iter().all(|r| r.ok), "{ok:?}");
        let bad = evaluate_scope(
            &rule,
            &[
                Change::Modified("src/cart.js".into()),
                Change::Modified("src/tax.js".into()),
                Change::Deleted("package.json".into()),
            ],
        );
        assert!(
            !bad[0].ok && bad[0].detail.contains("M src/tax.js") && bad[0].detail.contains("D package.json"),
            "{bad:?}"
        );
        assert!(!bad[1].ok, "3 > max 2");
    }

    #[test]
    fn copy_tree_copies_nested_files_and_overwrites() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(src.path().join("test")).unwrap();
        std::fs::write(src.path().join("test/h.test.js"), "hidden").unwrap();
        std::fs::create_dir_all(dst.path().join("test")).unwrap();
        std::fs::write(dst.path().join("test/h.test.js"), "agent weakened this").unwrap();
        let w = copy_tree(src.path(), dst.path()).unwrap();
        assert_eq!(w, vec![("test/h.test.js".to_string(), "hidden".to_string())]);
        assert_eq!(std::fs::read_to_string(dst.path().join("test/h.test.js")).unwrap(), "hidden");
        assert!(copy_tree(&src.path().join("nope"), dst.path()).is_err());
    }

    #[test]
    fn tail_keeps_the_end() {
        assert_eq!(tail("abc", 10), "abc");
        assert_eq!(tail("abcdef", 3), "…def");
    }

    // Spawns `sh`, which the Windows runner does not have on PATH.
    #[cfg(unix)]
    #[tokio::test]
    async fn command_check_reports_exit_status_and_output() {
        let d = tempfile::tempdir().unwrap();
        let pass = CommandCheck {
            run: "test \"$CI\" = 1".into(),
            timeout_s: 10,
            reason: "r".into(),
        };
        assert!(run_command(&pass, d.path()).await.ok, "CI=1 must be set for test runners");
        let fail = CommandCheck {
            run: "echo boom-output; exit 3".into(),
            timeout_s: 10,
            reason: "tests must pass".into(),
        };
        let r = run_command(&fail, d.path()).await;
        assert!(!r.ok);
        assert!(
            r.detail.contains("exited 3") && r.detail.contains("boom-output") && r.detail.contains("tests must pass"),
            "{}",
            r.detail
        );
    }

    // Spawns `sh`, which the Windows runner does not have on PATH.
    #[cfg(unix)]
    #[tokio::test]
    async fn command_check_times_out() {
        let d = tempfile::tempdir().unwrap();
        let slow = CommandCheck {
            run: "sleep 30".into(),
            timeout_s: 1,
            reason: "r".into(),
        };
        let r = run_command(&slow, d.path()).await;
        assert!(!r.ok && r.detail.contains("timed out"), "{}", r.detail);
    }

    // Spawns `sh`, which the Windows runner does not have on PATH.
    #[cfg(unix)]
    #[tokio::test]
    async fn serve_check_fails_cleanly_when_nothing_listens() {
        let d = tempfile::tempdir().unwrap();
        let s = ServeCheck {
            run: "sleep 30".into(),
            url: "http://127.0.0.1:9/".into(),
            contains: None,
            timeout_s: 2,
            reason: "app must boot".into(),
        };
        let r = run_serve(&s, d.path(), None).await;
        assert!(!r.ok && r.detail.contains("app must boot"), "{}", r.detail);
    }

    // Spawns `sh`, which the Windows runner does not have on PATH.
    #[cfg(unix)]
    #[tokio::test]
    async fn serve_check_passes_when_the_app_answers_and_tears_it_down() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("index.html"), "<div id=\"root\"></div>").unwrap();
        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let s = ServeCheck {
            run: format!("python3 -m http.server {port} --bind 127.0.0.1"),
            url: format!("http://127.0.0.1:{port}/"),
            contains: Some("root".into()),
            timeout_s: 20,
            reason: "app must boot".into(),
        };
        let r = run_serve(&s, d.path(), None).await;
        assert!(r.ok, "{}", r.detail);
        // The server's process group is killed once the check returns.
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert!(std::net::TcpStream::connect(("127.0.0.1", port)).is_err(), "serve process outlived the check");
    }
}

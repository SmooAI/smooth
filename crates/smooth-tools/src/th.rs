//! `th` — run the SmooAI operator CLI as a native tool.
//!
//! Big Smooth dogfoods `th`: instead of blindly shelling `bash` at the `th`
//! binary, the daemon gets a typed tool whose description maps the high-value
//! surface (web search, knowledge retrieval, crawl, the SmooAI API, pearls) so
//! the model knows *when* to reach for it. Args are passed verbatim as argv —
//! there is **no shell**, so no interpolation/injection path — only the `th`
//! binary with the caller's argument list.

use std::path::PathBuf;
use std::process::Stdio;

use async_trait::async_trait;
use serde_json::{json, Value};
use smooth_operator::{Tool, ToolSchema};

/// Max bytes returned per stream before truncation.
const OUTPUT_CAP: usize = 50_000;

/// `th` tool — invokes the SmooAI operator CLI with an argv array.
pub struct ThTool {
    /// Default working directory when the call doesn't override it with `cwd`.
    pub workspace: PathBuf,
}

/// The `th` executable's file name for this platform — `th` on Unix, `th.exe`
/// on Windows. Every non-env lookup below joins a directory with this, so the
/// binary is actually found on Windows (a bare `th` matches nothing there).
fn th_exe_name() -> String {
    format!("th{}", std::env::consts::EXE_SUFFIX)
}

/// Locate the `th` binary. Resolution order (mirrors `daemon_launcher`'s shape):
/// `SMOOTH_TH_BIN` env → next to the running executable → `~/.cargo/bin/th` →
/// `PATH`.
fn resolve_th() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("SMOOTH_TH_BIN") {
        let p = PathBuf::from(p);
        if p.is_file() {
            return Some(p);
        }
    }
    let exe_name = th_exe_name();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(p) = exe.parent().map(|d| d.join(&exe_name)) {
            if p.is_file() {
                return Some(p);
            }
        }
    }
    if let Some(p) = dirs_next::home_dir().map(|h| h.join(".cargo").join("bin").join(&exe_name)) {
        if p.is_file() {
            return Some(p);
        }
    }
    which_on_path()
}

fn which_on_path() -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    let exe_name = th_exe_name();
    std::env::split_paths(&path).map(|d| d.join(&exe_name)).find(|p| p.is_file())
}

#[async_trait]
impl Tool for ThTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "th".into(),
            description: "Run the `th` CLI (the SmooAI operator) with an argv array. Prefer this over shelling `bash th …`. Use it for:\n\
                - web search: [\"search\", \"<query>\"] (add \"--answer\" for a synthesized answer)\n\
                - knowledge-base retrieval over the org's OWN docs: [\"knowledge\", \"search\", \"<query>\"]\n\
                - web crawl / scrape a page to clean markdown: [\"crawl\", \"scrape\", \"<url>\"]\n\
                - SmooAI platform API: [\"api\", \"<resource>\", \"<verb>\", …] where resource ∈ agents, crm, knowledge, jobs, keys, members, config, observability, testing, profile\n\
                - pearls (work-item tracker): [\"pearls\", \"list\"|\"show\"|\"ready\", …]\n\
                - config values: [\"config\", \"get\"|\"list\", …]; org context: [\"org\", \"list\"], [\"api\", \"whoami\"]\n\
                Pass args as a JSON array of strings passed verbatim to `th` (no shell, no interpolation). Example: [\"search\", \"best rust http client\"]. \
                Run [\"--help\"] or [\"<command>\", \"--help\"] to discover exact flags — every subcommand is self-documenting. \
                This can hit the network and the SmooAI API; that is intended."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "args": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Arguments passed verbatim as argv to `th`, e.g. [\"search\", \"rust async runtime\"]."
                    },
                    "cwd": {
                        "type": "string",
                        "description": "Optional working directory to run `th` in (default: the workspace)."
                    }
                },
                "required": ["args"]
            }),
        }
    }

    fn is_concurrent_safe(&self) -> bool {
        // `th` args can mutate (api verbs, pearls create/close), so don't run it
        // concurrently with other tools.
        false
    }

    async fn execute(&self, arguments: Value) -> anyhow::Result<String> {
        let args = parse_args(&arguments)?;
        let cwd = arguments
            .get("cwd")
            .and_then(Value::as_str)
            .map_or_else(|| self.workspace.clone(), PathBuf::from);
        run_th(&args, &cwd).await
    }
}

/// Resolve and run the `th` binary with `args` in `cwd`, returning a formatted
/// `$ th … / exit code / stdout / stderr` string (streams truncated at
/// [`OUTPUT_CAP`]). Shared by [`ThTool`] and the typed convenience tools
/// (`web_search`, …) that shell specific `th` subcommands. No shell — argv only.
pub(crate) async fn run_th(args: &[String], cwd: &std::path::Path) -> anyhow::Result<String> {
    let bin = resolve_th()
        .ok_or_else(|| anyhow::anyhow!("could not find the `th` binary (looked at SMOOTH_TH_BIN, next to the executable, ~/.cargo/bin/th, and PATH)"))?;

    let mut cmd = tokio::process::Command::new(&bin);
    cmd.args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let output = cmd
        .spawn()
        .map_err(|e| anyhow::anyhow!("failed to spawn `th`: {e}"))?
        .wait_with_output()
        .await
        .map_err(|e| anyhow::anyhow!("`th` error: {e}"))?;

    let code = output.status.code().map_or_else(|| "killed by signal".to_owned(), |c| c.to_string());
    let stdout = truncate(&String::from_utf8_lossy(&output.stdout));
    let stderr = truncate(&String::from_utf8_lossy(&output.stderr));
    Ok(format!(
        "$ th {}\nexit code: {code}\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}",
        args.join(" ")
    ))
}

/// Locate the `th` binary (see [`resolve_th`]). Exposed for sibling typed tools.
pub(crate) fn th_is_resolvable() -> bool {
    resolve_th().is_some()
}

/// Run `th <args>` and return its stdout **only on clean success** (exit 0,
/// non-blank stdout). Returns `None` when `th` is missing, exits non-zero
/// (e.g. not logged in / api.smoo.ai unreachable), or prints nothing — the
/// caller's cue to fall back. Unlike [`run_th`] this returns the raw stdout,
/// not the `$ th … / exit / stdout / stderr` frame, so results go straight to
/// the model.
pub(crate) async fn capture_th(args: &[String], cwd: &std::path::Path) -> Option<String> {
    let bin = resolve_th()?;
    let output = tokio::process::Command::new(&bin)
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .output()
        .await
        .ok()?;
    accept_stdout(output.status.success(), &output.stdout)
}

/// Accept a subprocess' stdout only on clean success with non-blank output
/// (truncated to [`OUTPUT_CAP`]); otherwise `None`. Split out from
/// [`capture_th`] so the decision is testable without spawning a process.
fn accept_stdout(success: bool, stdout: &[u8]) -> Option<String> {
    if !success {
        return None;
    }
    let stdout = truncate(&String::from_utf8_lossy(stdout));
    (!stdout.trim().is_empty()).then_some(stdout)
}

/// Extract the required `args` array as a `Vec<String>`, rejecting non-string
/// elements. At least one arg is required.
fn parse_args(arguments: &Value) -> anyhow::Result<Vec<String>> {
    let arr = arguments
        .get("args")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("missing required array parameter `args`"))?;
    if arr.is_empty() {
        anyhow::bail!("`args` must contain at least one argument");
    }
    arr.iter()
        .map(|v| {
            v.as_str()
                .map(str::to_owned)
                .ok_or_else(|| anyhow::anyhow!("every element of `args` must be a string"))
        })
        .collect()
}

fn truncate(s: &str) -> String {
    if s.len() <= OUTPUT_CAP {
        return s.to_owned();
    }
    let mut end = OUTPUT_CAP;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n... (truncated, {} bytes total)", &s[..end], s.len())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, reason = "unwrap/expect are the idiom for test assertions")]
mod tests {
    use super::*;

    fn tool() -> ThTool {
        ThTool {
            workspace: std::env::temp_dir(),
        }
    }

    #[test]
    fn parse_args_extracts_strings() {
        let args = parse_args(&json!({"args": ["search", "rust http client"]})).unwrap();
        assert_eq!(args, vec!["search", "rust http client"]);
    }

    #[test]
    fn parse_args_rejects_missing() {
        assert!(parse_args(&json!({})).is_err());
    }

    #[test]
    fn parse_args_rejects_empty() {
        assert!(parse_args(&json!({"args": []})).is_err());
    }

    #[test]
    fn parse_args_rejects_non_string_elements() {
        assert!(parse_args(&json!({"args": ["search", 7]})).is_err());
    }

    #[test]
    fn accept_stdout_keeps_clean_success_and_rejects_the_rest() {
        // Clean success with real output → kept (this is the cluster-hit path).
        assert_eq!(accept_stdout(true, b"results\n").as_deref(), Some("results\n"));
        // Non-zero exit (not logged in / unreachable) → fall back.
        assert_eq!(accept_stdout(false, b"results"), None);
        // Success but empty/blank stdout → nothing to show → fall back.
        assert_eq!(accept_stdout(true, b""), None);
        assert_eq!(accept_stdout(true, b"   \n"), None);
    }

    /// Every non-env lookup in `resolve_th` joins a directory with this name,
    /// so it has to carry the platform's executable extension — a bare `th`
    /// matches nothing on Windows, and the tool then reported "could not find
    /// the `th` binary" on a host where `th.exe` was sitting right there.
    #[test]
    fn th_exe_name_carries_the_platform_suffix() {
        assert_eq!(th_exe_name(), format!("th{}", std::env::consts::EXE_SUFFIX));
        if cfg!(target_os = "windows") {
            assert_eq!(th_exe_name(), "th.exe");
        } else {
            assert_eq!(th_exe_name(), "th");
        }
    }

    #[test]
    fn schema_advertises_the_th_name() {
        let s = tool().schema();
        assert_eq!(s.name, "th");
        assert!(s.description.contains("search"), "description maps the surface");
        assert!(!tool().is_concurrent_safe());
    }

    #[tokio::test]
    async fn missing_args_is_an_error() {
        assert!(tool().execute(json!({})).await.is_err());
    }

    /// End-to-end: `th --version` succeeds when the binary is resolvable.
    /// Gated on the binary being present so the suite passes in bare CI.
    ///
    /// Ignored on Windows for pearl th-bd84cf: rendering help/version for th's
    /// 53-command clap tree overflows the 1 MB Windows main-thread stack, so
    /// the child dies with 0xC00000FD before printing. Pre-existing and
    /// reproduced on plain `main` (PR #331 probe) — it only started firing here
    /// because a new `CARGO_BIN_EXE_th` test puts `th.exe` where `resolve_th()`
    /// finds it, so this stopped skipping. Un-ignore once th-bd84cf lands.
    #[tokio::test]
    #[cfg_attr(windows, ignore = "pearl th-bd84cf: clap help/version overflows the 1 MB Windows main stack")]
    async fn version_runs_when_th_is_installed() {
        if resolve_th().is_none() {
            eprintln!("skipping: `th` not resolvable in this environment");
            return;
        }
        let out = tool().execute(json!({"args": ["--version"]})).await.unwrap();
        assert!(out.contains("exit code: 0"), "{out}");
        assert!(out.contains("th "), "version output should name th: {out}");
    }

    /// Error path: an unknown subcommand surfaces a non-zero exit + stderr.
    ///
    /// Also ignored on Windows for th-bd84cf, though it *passes* there — a
    /// stack overflow exits non-zero too, so the assertion holds for the wrong
    /// reason. A test that is green only because the binary crashed is worse
    /// than one that is skipped.
    #[tokio::test]
    #[cfg_attr(windows, ignore = "pearl th-bd84cf: passes only vacuously — the crash exit is also non-zero")]
    async fn unknown_subcommand_surfaces_nonzero_exit() {
        if resolve_th().is_none() {
            eprintln!("skipping: `th` not resolvable in this environment");
            return;
        }
        let out = tool().execute(json!({"args": ["definitely-not-a-real-subcommand-xyzzy"]})).await.unwrap();
        assert!(!out.contains("exit code: 0"), "unknown subcommand should fail: {out}");
    }
}

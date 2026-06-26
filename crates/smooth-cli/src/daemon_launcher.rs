//! `th daemon …` launcher — resolves and spawns the standalone `smooth-daemon`
//! binary instead of statically linking it into `th`.
//!
//! Keeping the operator runtime out of `th` is deliberate: most users install
//! `th` for `api`/`jira`/`pearls`/`config` and shouldn't pay for the embedded
//! operator (axum + the engine + adapters + the widget bundle). The daemon is a
//! separate artifact, resolved at first use and (if absent) downloaded from the
//! GitHub release — the same "auxiliary native binary" shape `th` already uses
//! for `smooth-dolt` / `smooth-operative`.
//!
//! `th daemon <args…>` is a **pure passthrough**: the args are forwarded to
//! `smooth-daemon <args…>`, which owns the full daemon CLI (`run` / `operator` /
//! `status` / `audit` / `schedule`). `th daemon --help` shows the binary's help.

use std::path::PathBuf;
use std::process::Command;

use anyhow::{Context, Result};

const BIN: &str = "smooth-daemon";
/// Where the on-demand download lands (and the first place we look after env).
fn install_dir() -> Option<PathBuf> {
    dirs_next::home_dir().map(|h| h.join(".smooth").join("bin"))
}

/// The Rust target triple for the running host, used to pick the release asset.
fn current_target() -> &'static str {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => "aarch64-apple-darwin",
        ("macos", "x86_64") => "x86_64-apple-darwin",
        ("linux", "aarch64") => "aarch64-unknown-linux-gnu",
        ("linux", "x86_64") => "x86_64-unknown-linux-gnu",
        ("windows", _) => "x86_64-pc-windows-msvc",
        _ => "unknown",
    }
}

/// Locate an existing `smooth-daemon` without downloading. Resolution order:
/// `SMOOTH_DAEMON_BIN` env → `~/.smooth/bin` → next to the running `th` →
/// `PATH` → the dev workspace `target/{release,debug}`.
fn find_existing() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("SMOOTH_DAEMON_BIN") {
        let p = PathBuf::from(p);
        if p.is_file() {
            return Some(p);
        }
    }
    if let Some(p) = install_dir().map(|d| d.join(BIN)) {
        if p.is_file() {
            return Some(p);
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(p) = exe.parent().map(|d| d.join(BIN)) {
            if p.is_file() {
                return Some(p);
            }
        }
    }
    if let Some(p) = which_on_path() {
        return Some(p);
    }
    // Dev workspace: walk up from the manifest dir looking for target/{release,debug}.
    if let Ok(manifest) = std::env::var("CARGO_MANIFEST_DIR") {
        let mut dir = PathBuf::from(manifest);
        for _ in 0..6 {
            for profile in ["release", "debug"] {
                let cand = dir.join("target").join(profile).join(BIN);
                if cand.is_file() {
                    return Some(cand);
                }
            }
            if !dir.pop() {
                break;
            }
        }
    }
    None
}

fn which_on_path() -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join(BIN)).find(|p| p.is_file())
}

/// Download `smooth-daemon` for the current platform from the latest GitHub
/// release into `~/.smooth/bin/`. The release ships a raw per-target asset named
/// `smooth-daemon-<target>` (`.exe` on Windows). Best-effort: returns an error
/// (with a build hint) when offline or the asset isn't published yet.
async fn download() -> Result<PathBuf> {
    let target = current_target();
    anyhow::ensure!(
        target != "unknown",
        "no smooth-daemon release asset for this platform ({}/{}); build it with `pnpm build:smooth-daemon`",
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    let suffix = if std::env::consts::OS == "windows" { ".exe" } else { "" };
    let asset = format!("{BIN}-{target}{suffix}");
    let url = format!("https://github.com/SmooAI/smooth/releases/latest/download/{asset}");

    let dir = install_dir().context("no home dir for ~/.smooth/bin")?;
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let dest = dir.join(BIN);

    eprintln!("th: fetching {BIN} ({target}) → {} …", dest.display());
    let client = reqwest::Client::builder().build()?;
    let resp = client.get(&url).send().await.with_context(|| format!("downloading {url}"))?;
    anyhow::ensure!(
        resp.status().is_success(),
        "could not download {asset} (HTTP {}). Build it locally: `pnpm build:smooth-daemon`, or set SMOOTH_DAEMON_BIN.",
        resp.status()
    );
    let bytes = resp.bytes().await.context("reading download body")?;
    std::fs::write(&dest, &bytes).with_context(|| format!("writing {}", dest.display()))?;
    make_executable(&dest);
    Ok(dest)
}

#[cfg(unix)]
fn make_executable(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755));
}
#[cfg(not(unix))]
fn make_executable(_path: &std::path::Path) {}

/// Resolve `smooth-daemon`, downloading it on demand if not already present.
async fn resolve() -> Result<PathBuf> {
    if let Some(p) = find_existing() {
        return Ok(p);
    }
    download().await
}

/// `th daemon <args…>` — resolve + spawn the standalone daemon binary, inheriting
/// stdio and propagating its exit code.
pub async fn run(args: Vec<String>) -> Result<()> {
    let bin = resolve().await?;
    let status = Command::new(&bin).args(&args).status().with_context(|| format!("spawning {}", bin.display()))?;
    if !status.success() {
        // Mirror the child's exit so scripts see the real code.
        std::process::exit(status.code().unwrap_or(1));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_is_known_on_supported_hosts() {
        // The CI hosts (mac arm64 / linux x64) must map to a real triple.
        if matches!(std::env::consts::OS, "macos" | "linux") {
            assert_ne!(current_target(), "unknown");
        }
    }

    #[test]
    fn install_dir_is_under_dot_smooth() {
        if let Some(d) = install_dir() {
            assert!(d.ends_with(".smooth/bin"));
        }
    }
}

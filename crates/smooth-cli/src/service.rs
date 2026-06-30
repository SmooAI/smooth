//! `th service` — keep `th up` running across reboots via the native
//! service manager on each platform.
//!
//! User-level by default:
//! - **macOS**: LaunchAgent plist at `~/Library/LaunchAgents/com.smooai.smooth.plist`
//!   driven by `launchctl bootstrap gui/$UID` / `bootout`.
//! - **Linux**: systemd user unit at `~/.config/systemd/user/smooth.service`
//!   driven by `systemctl --user`. `loginctl enable-linger` is recommended
//!   (printed as a hint) so the service survives logout.
//! - **Windows**: logon-triggered Scheduled Task via `schtasks`.
//!
//! `--system` prints the system-level artifact + instructions to stdout
//! instead of installing — system-level installs need sudo / Administrator
//! and vary more across distros, so we don't try to automate them.

use std::path::PathBuf;

use anyhow::{Context, Result};
use owo_colors::OwoColorize;

pub const LABEL: &str = "com.smooai.smooth";
/// The continuous-update timer's LaunchAgent label (macOS).
pub const UPDATER_LABEL: &str = "com.smooai.smooth.updater";

/// Parse `KEY=VALUE` strings (from `--env`) into pairs. Errors on a missing `=`
/// or an empty key so a typo fails loudly rather than baking junk into a plist.
pub fn parse_env(raw: &[String]) -> Result<Vec<(String, String)>> {
    raw.iter()
        .map(|s| {
            let (k, v) = s.split_once('=').with_context(|| format!("--env must be KEY=VALUE, got {s:?}"))?;
            let k = k.trim();
            if k.is_empty() {
                anyhow::bail!("--env has an empty key: {s:?}");
            }
            Ok((k.to_string(), v.to_string()))
        })
        .collect()
}

/// The git checkout `self-update` pulls from: `--repo`, else `SMOOTH_UPDATE_REPO`,
/// else `~/dev/smooai/smooth`. Validated as an existing git work tree.
fn resolve_update_repo(repo: Option<PathBuf>) -> Result<PathBuf> {
    let path = repo
        .or_else(|| std::env::var_os("SMOOTH_UPDATE_REPO").map(PathBuf::from))
        .or_else(|| dirs_next::home_dir().map(|h| h.join("dev").join("smooai").join("smooth")))
        .context("cannot determine update repo (pass --repo)")?;
    if !path.join(".git").exists() {
        anyhow::bail!("{} is not a git checkout (pass --repo to point at your smooth clone)", path.display());
    }
    Ok(path)
}

pub fn install(system: bool, daemon: bool, env: &[(String, String)]) -> Result<()> {
    let exe = std::env::current_exe().context("resolving current `th` executable path")?;
    let home = dirs_next::home_dir().context("cannot determine home directory")?;
    let log_path = home.join(".smooth").join("service.log");
    let err_path = home.join(".smooth").join("service.err");
    std::fs::create_dir_all(home.join(".smooth"))?;

    if system {
        print_system_artifact(&exe, &home, &log_path, &err_path, daemon, env);
        return Ok(());
    }

    #[cfg(target_os = "macos")]
    {
        macos::install_user(&exe, &log_path, &err_path, daemon, env)
    }
    #[cfg(target_os = "linux")]
    {
        linux::install_user(&exe, &log_path, &err_path, &home, daemon, env)
    }
    #[cfg(target_os = "windows")]
    {
        let _ = (log_path, err_path, env);
        windows::install_user(&exe, daemon)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        let _ = (exe, home, log_path, err_path, daemon, env);
        anyhow::bail!("th service is not implemented on this platform")
    }
}

/// Pull latest source, rebuild + reinstall (`pnpm install:th`), then restart the
/// running service so it picks up the new `th` + daemon binaries.
pub fn self_update(repo: Option<PathBuf>) -> Result<()> {
    let repo = resolve_update_repo(repo)?;
    println!("\n  {} Updating from {}", "↻".cyan(), repo.display().to_string().dimmed());

    run_in("git", &["pull", "--rebase", "--autostash"], &repo).context("git pull")?;
    run_in("pnpm", &["install:th"], &repo).context("pnpm install:th (rebuild + reinstall th + daemon)")?;

    // Restart the daemon service so it execs the freshly-built binaries. Best
    // effort: if the service isn't installed, just note it.
    match restart() {
        Ok(()) => println!("  {} Updated + restarted the service.\n", "✓".green().bold()),
        Err(e) => println!("  {} Updated; service not restarted ({e}). Run `th service restart`.\n", "ℹ".cyan()),
    }
    Ok(())
}

/// Install the continuous-update timer (runs `th service self-update` on an
/// interval). macOS only for now; other platforms print guidance.
pub fn install_updater(repo: Option<PathBuf>, interval: u64) -> Result<()> {
    let repo = resolve_update_repo(repo)?;
    let exe = std::env::current_exe().context("resolving current `th` executable path")?;
    #[cfg(target_os = "macos")]
    {
        macos::install_updater(&exe, &repo, interval)
    }
    #[cfg(not(target_os = "macos"))]
    {
        println!(
            "\n  {} Automated updater is macOS-only for now. On Linux, add a systemd timer that runs:\n    {}\n",
            "ℹ".cyan(),
            format!("{} service self-update --repo {}", exe.display(), repo.display()).cyan()
        );
        let _ = interval;
        Ok(())
    }
}

/// Run a command in `dir`, streaming its output; error on non-zero exit.
fn run_in(program: &str, args: &[&str], dir: &std::path::Path) -> Result<()> {
    let status = std::process::Command::new(program)
        .args(args)
        .current_dir(dir)
        .status()
        .with_context(|| format!("spawn `{program} {}`", args.join(" ")))?;
    if !status.success() {
        anyhow::bail!("`{program} {}` exited {status}", args.join(" "));
    }
    Ok(())
}

pub fn uninstall() -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        macos::uninstall_user()
    }
    #[cfg(target_os = "linux")]
    {
        linux::uninstall_user()
    }
    #[cfg(target_os = "windows")]
    {
        windows::uninstall_user()
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        anyhow::bail!("th service is not implemented on this platform")
    }
}

pub fn start() -> Result<()> {
    platform_control("start")
}

pub fn stop() -> Result<()> {
    platform_control("stop")
}

pub fn restart() -> Result<()> {
    platform_control("restart")
}

pub fn status() -> Result<()> {
    platform_control("status")
}

pub fn logs(follow: bool) -> Result<()> {
    let home = dirs_next::home_dir().context("cannot determine home directory")?;
    let log = home.join(".smooth").join("service.log");
    let err = home.join(".smooth").join("service.err");

    if !log.exists() && !err.exists() {
        println!(
            "\n  {} No service logs yet at {} — is the service installed and started?\n",
            "ℹ".cyan(),
            log.display().to_string().dimmed()
        );
        return Ok(());
    }

    // `tail -F` gracefully handles files appearing/rotating. Prefer it
    // over re-implementing in Rust.
    let mut args: Vec<String> = Vec::new();
    if follow {
        args.push("-F".to_string());
    } else {
        args.push("-n".to_string());
        args.push("200".to_string());
    }
    args.push(log.to_string_lossy().to_string());
    if err.exists() {
        args.push(err.to_string_lossy().to_string());
    }

    let status = std::process::Command::new("tail")
        .args(&args)
        .status()
        .context("spawn `tail` to read service logs")?;
    if !status.success() {
        anyhow::bail!("tail exited {status}");
    }
    Ok(())
}

fn platform_control(action: &str) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        macos::control(action)
    }
    #[cfg(target_os = "linux")]
    {
        linux::control(action)
    }
    #[cfg(target_os = "windows")]
    {
        windows::control(action)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        let _ = action;
        anyhow::bail!("th service is not implemented on this platform")
    }
}

fn print_system_artifact(exe: &std::path::Path, home: &std::path::Path, log: &std::path::Path, err: &std::path::Path, daemon: bool, env: &[(String, String)]) {
    println!("\n  {} System-level install prints the artifact; install it manually.\n", "ℹ".cyan());
    #[cfg(target_os = "macos")]
    {
        let plist = macos::render_plist(exe, log, err, daemon, env);
        println!("  Save the following to {}:\n", "/Library/LaunchDaemons/com.smooai.smooth.plist".cyan());
        println!("{plist}");
        println!("\n  Then:\n");
        println!("    {}", "sudo chown root:wheel /Library/LaunchDaemons/com.smooai.smooth.plist".cyan());
        println!(
            "    {}",
            "sudo launchctl bootstrap system /Library/LaunchDaemons/com.smooai.smooth.plist".cyan()
        );
        println!();
    }
    #[cfg(target_os = "linux")]
    {
        let unit = linux::render_unit(exe, log, err, daemon, env);
        println!("  Save the following to {}:\n", "/etc/systemd/system/smooth.service".cyan());
        println!("{unit}");
        println!("\n  Then:\n");
        println!("    {}", "sudo systemctl daemon-reload".cyan());
        println!("    {}", "sudo systemctl enable --now smooth.service".cyan());
        println!();
    }
    #[cfg(target_os = "windows")]
    {
        let _ = (exe, log, err);
        println!("  On Windows, use `schtasks` under an elevated shell:");
        println!(
            "    {}",
            format!("schtasks /Create /SC ONSTART /RU SYSTEM /TN SmoothAI /TR \"{}\" /RL HIGHEST", exe.display()).cyan()
        );
        println!();
    }
    let _ = (home, daemon, env);
}

// ---------------------------------------------------------------------------
// macOS — launchd
// ---------------------------------------------------------------------------

#[cfg(target_os = "macos")]
mod macos {
    use super::{Context, OwoColorize, PathBuf, Result, LABEL};

    fn plist_path() -> Result<PathBuf> {
        let home = dirs_next::home_dir().context("home directory")?;
        Ok(home.join("Library").join("LaunchAgents").join(format!("{LABEL}.plist")))
    }

    /// Render extra `<key>/<string>` env entries for the plist's
    /// `EnvironmentVariables` dict (each indented to sit inside it).
    fn env_entries(env: &[(String, String)], esc: &impl Fn(&str) -> String) -> String {
        env.iter()
            .map(|(k, v)| format!("        <key>{}</key>\n        <string>{}</string>\n", esc(k), esc(v)))
            .collect()
    }

    pub fn render_plist(exe: &std::path::Path, log: &std::path::Path, err: &std::path::Path, daemon: bool, env: &[(String, String)]) -> String {
        let program_args = if daemon {
            "        <string>daemon</string>".to_string()
        } else {
            "        <string>up</string>\n        <string>--foreground</string>".to_string()
        };
        // Escape minimal XML-unsafe chars. Paths with & < > in them are
        // vanishingly rare on macOS but may as well be correct.
        let esc = |s: &str| s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
        let home = dirs_next::home_dir().map(|h| h.display().to_string()).unwrap_or_else(|| "/tmp".to_string());
        let extra_env = env_entries(env, &esc);
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{label}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{exe}</string>
{program_args}
    </array>
    <key>WorkingDirectory</key>
    <string>{home}</string>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>ThrottleInterval</key>
    <integer>10</integer>
    <key>StandardOutPath</key>
    <string>{log}</string>
    <key>StandardErrorPath</key>
    <string>{err}</string>
    <key>EnvironmentVariables</key>
    <dict>
        <key>PATH</key>
        <string>/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin</string>
        <key>HOME</key>
        <string>{home}</string>
{extra_env}    </dict>
</dict>
</plist>
"#,
            label = LABEL,
            exe = esc(&exe.display().to_string()),
            log = esc(&log.display().to_string()),
            err = esc(&err.display().to_string()),
            home = esc(&home),
            extra_env = extra_env,
        )
    }

    pub fn install_user(exe: &std::path::Path, log: &std::path::Path, err: &std::path::Path, daemon: bool, env: &[(String, String)]) -> Result<()> {
        let path = plist_path()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let body = render_plist(exe, log, err, daemon, env);
        std::fs::write(&path, body).with_context(|| format!("write {}", path.display()))?;

        // If there's a neighbor smooth-dolt (common for scp'd installs:
        // `cargo install --path` for th, plus a manual scp of smooth-dolt
        // from another machine), re-adhoc-sign it. macOS launchd rejects
        // binaries carrying a signature from a different machine with
        // "Malformed Mach-o file (os error 88)" even though they run fine
        // from an interactive shell. A fresh adhoc sign on the target
        // machine resolves it and is idempotent on already-signed binaries.
        if let Some(dir) = exe.parent() {
            let dolt = dir.join("smooth-dolt");
            if dolt.is_file() {
                resign_adhoc(&dolt);
            }
        }

        let uid = current_uid()?;
        let domain = format!("gui/{uid}");

        // Best-effort: bootout first so re-install is idempotent.
        let _ = run_launchctl(&["bootout", &domain, &path.display().to_string()]);
        run_launchctl(&["bootstrap", &domain, &path.display().to_string()])?;

        println!("\n  {} Installed LaunchAgent at {}", "✓".green().bold(), path.display().to_string().dimmed());
        println!(
            "  {} {} to start / {} to stop / {} to view logs\n",
            "→".dimmed(),
            "th service start".cyan(),
            "th service stop".cyan(),
            "th service logs".cyan()
        );
        Ok(())
    }

    /// Best-effort adhoc re-sign. Swallows failures (missing `codesign`,
    /// read-only binary) — install should still succeed. If the binary
    /// was fine before, we just re-sign with the same kind of ad-hoc
    /// identity; no effective change.
    fn resign_adhoc(binary: &std::path::Path) {
        let out = std::process::Command::new("codesign").args(["--force", "--sign", "-"]).arg(binary).output();
        match out {
            Ok(o) if o.status.success() => {
                println!(
                    "  {} Re-signed {} (adhoc)",
                    "✓".green().bold(),
                    binary.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default().dimmed()
                );
            }
            Ok(o) => {
                let stderr = String::from_utf8_lossy(&o.stderr);
                tracing::warn!(binary = %binary.display(), stderr = %stderr, "adhoc re-sign failed");
            }
            Err(e) => {
                tracing::warn!(binary = %binary.display(), error = %e, "codesign not available");
            }
        }
    }

    fn updater_plist_path() -> Result<PathBuf> {
        let home = dirs_next::home_dir().context("home directory")?;
        Ok(home.join("Library").join("LaunchAgents").join(format!("{}.plist", super::UPDATER_LABEL)))
    }

    /// A LaunchAgent that runs `th service self-update --repo <repo>` every
    /// `interval` seconds. `RunAtLoad` is false (don't rebuild the instant it's
    /// installed); the inherited PATH covers git/pnpm/cargo.
    pub fn render_updater_plist(exe: &std::path::Path, repo: &std::path::Path, interval: u64, log: &std::path::Path, err: &std::path::Path) -> String {
        let esc = |s: &str| s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
        let home = dirs_next::home_dir().map(|h| h.display().to_string()).unwrap_or_else(|| "/tmp".to_string());
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{label}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{exe}</string>
        <string>service</string>
        <string>self-update</string>
        <string>--repo</string>
        <string>{repo}</string>
    </array>
    <key>WorkingDirectory</key>
    <string>{repo}</string>
    <key>RunAtLoad</key>
    <false/>
    <key>StartInterval</key>
    <integer>{interval}</integer>
    <key>StandardOutPath</key>
    <string>{log}</string>
    <key>StandardErrorPath</key>
    <string>{err}</string>
    <key>EnvironmentVariables</key>
    <dict>
        <key>PATH</key>
        <string>{home}/.cargo/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin</string>
        <key>HOME</key>
        <string>{home}</string>
    </dict>
</dict>
</plist>
"#,
            label = super::UPDATER_LABEL,
            exe = esc(&exe.display().to_string()),
            repo = esc(&repo.display().to_string()),
            log = esc(&log.display().to_string()),
            err = esc(&err.display().to_string()),
            home = esc(&home),
        )
    }

    pub fn install_updater(exe: &std::path::Path, repo: &std::path::Path, interval: u64) -> Result<()> {
        let home = dirs_next::home_dir().context("home directory")?;
        let log = home.join(".smooth").join("updater.log");
        let err = home.join(".smooth").join("updater.err");
        std::fs::create_dir_all(home.join(".smooth"))?;
        let path = updater_plist_path()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, render_updater_plist(exe, repo, interval, &log, &err)).with_context(|| format!("write {}", path.display()))?;

        let uid = current_uid()?;
        let domain = format!("gui/{uid}");
        let _ = run_launchctl(&["bootout", &domain, &path.display().to_string()]);
        run_launchctl(&["bootstrap", &domain, &path.display().to_string()])?;

        println!(
            "\n  {} Installed updater LaunchAgent (every {interval}s) at {}",
            "✓".green().bold(),
            path.display().to_string().dimmed()
        );
        println!("  {} updates from {}\n", "→".dimmed(), repo.display().to_string().cyan());
        Ok(())
    }

    pub fn uninstall_user() -> Result<()> {
        // Also remove the updater agent if present.
        if let Ok(up) = updater_plist_path() {
            if up.exists() {
                if let Ok(uid) = current_uid() {
                    let _ = run_launchctl(&["bootout", &format!("gui/{uid}"), &up.display().to_string()]);
                }
                let _ = std::fs::remove_file(&up);
            }
        }
        let path = plist_path()?;
        if !path.exists() {
            println!("\n  {} No LaunchAgent installed at {}\n", "ℹ".cyan(), path.display().to_string().dimmed());
            return Ok(());
        }
        let uid = current_uid()?;
        let domain = format!("gui/{uid}");
        let _ = run_launchctl(&["bootout", &domain, &path.display().to_string()]);
        std::fs::remove_file(&path).with_context(|| format!("remove {}", path.display()))?;
        println!("\n  {} Removed LaunchAgent at {}\n", "✓".green().bold(), path.display().to_string().dimmed());
        Ok(())
    }

    pub fn control(action: &str) -> Result<()> {
        let uid = current_uid()?;
        let service = format!("gui/{uid}/{LABEL}");
        match action {
            "start" => run_launchctl(&["kickstart", "-k", &service]).map(|()| report_action("started")),
            "stop" => run_launchctl(&["kill", "SIGTERM", &service]).map(|()| report_action("stopped")),
            "restart" => run_launchctl(&["kickstart", "-k", &service]).map(|()| report_action("restarted")),
            "status" => {
                let out = std::process::Command::new("launchctl")
                    .args(["print", &service])
                    .output()
                    .context("launchctl print")?;
                if !out.status.success() {
                    println!("\n  {} LaunchAgent not loaded. Run `th service install` first.\n", "✗".red().bold());
                    return Ok(());
                }
                println!("{}", String::from_utf8_lossy(&out.stdout));
                Ok(())
            }
            other => anyhow::bail!("unknown action: {other}"),
        }
    }

    fn run_launchctl(args: &[&str]) -> Result<()> {
        let out = std::process::Command::new("launchctl").args(args).output().context("spawn launchctl")?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr).to_string();
            anyhow::bail!("launchctl {} failed: {}", args.join(" "), stderr.trim());
        }
        Ok(())
    }

    fn report_action(verb: &str) {
        println!("\n  {} Service {verb}.\n", "✓".green().bold());
    }

    /// Shell out to `id -u` to get the current uid. Avoids pulling in
    /// `libc` or `nix` and complies with the workspace's
    /// `unsafe_code = "forbid"` lint.
    fn current_uid() -> Result<u32> {
        let out = std::process::Command::new("id").arg("-u").output().context("spawn `id -u`")?;
        if !out.status.success() {
            anyhow::bail!("`id -u` exited {}", out.status);
        }
        let s = String::from_utf8_lossy(&out.stdout);
        s.trim().parse::<u32>().with_context(|| format!("parse uid from `id -u` output `{s}`"))
    }
}

// ---------------------------------------------------------------------------
// Linux — systemd --user
// ---------------------------------------------------------------------------

#[cfg(target_os = "linux")]
mod linux {
    use super::{Context, OwoColorize, PathBuf, Result, LABEL};

    pub const UNIT_NAME: &str = "smooth.service";

    fn unit_path(home: &std::path::Path) -> PathBuf {
        home.join(".config").join("systemd").join("user").join(UNIT_NAME)
    }

    pub fn render_unit(exe: &std::path::Path, log: &std::path::Path, err: &std::path::Path, daemon: bool, env: &[(String, String)]) -> String {
        let exec_cmd = if daemon {
            format!("{} daemon", exe.display())
        } else {
            format!("{} up --foreground", exe.display())
        };
        let env_lines: String = env.iter().map(|(k, v)| format!("Environment=\"{k}={v}\"\n")).collect();
        format!(
            r#"[Unit]
Description=Smooth (Smoo AI orchestration)
After=network.target

[Service]
Type=simple
ExecStart={exec_cmd}
WorkingDirectory=%h
Restart=on-failure
RestartSec=3s
{env_lines}StandardOutput=append:{log}
StandardError=append:{err}

[Install]
WantedBy=default.target
"#,
            log = log.display(),
            err = err.display(),
        )
    }

    pub fn install_user(
        exe: &std::path::Path,
        log: &std::path::Path,
        err: &std::path::Path,
        home: &std::path::Path,
        daemon: bool,
        env: &[(String, String)],
    ) -> Result<()> {
        let path = unit_path(home);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, render_unit(exe, log, err, daemon, env)).with_context(|| format!("write {}", path.display()))?;

        run_systemctl(&["--user", "daemon-reload"])?;
        run_systemctl(&["--user", "enable", "--now", UNIT_NAME])?;

        println!(
            "\n  {} Installed systemd user unit at {}",
            "✓".green().bold(),
            path.display().to_string().dimmed()
        );
        println!(
            "  {} run {} so the service survives logout\n",
            "→".dimmed(),
            format!("sudo loginctl enable-linger {}", std::env::var("USER").unwrap_or_default()).cyan()
        );
        Ok(())
    }

    pub fn uninstall_user() -> Result<()> {
        let home = dirs_next::home_dir().context("home directory")?;
        let path = unit_path(&home);
        if !path.exists() {
            println!("\n  {} No user unit at {}\n", "ℹ".cyan(), path.display().to_string().dimmed());
            return Ok(());
        }
        let _ = run_systemctl(&["--user", "disable", "--now", UNIT_NAME]);
        std::fs::remove_file(&path).with_context(|| format!("remove {}", path.display()))?;
        let _ = run_systemctl(&["--user", "daemon-reload"]);
        println!(
            "\n  {} Removed systemd user unit at {}\n",
            "✓".green().bold(),
            path.display().to_string().dimmed()
        );
        Ok(())
    }

    pub fn control(action: &str) -> Result<()> {
        match action {
            "start" | "stop" | "restart" | "status" => run_systemctl(&["--user", action, UNIT_NAME]),
            other => anyhow::bail!("unknown action: {other}"),
        }
    }

    fn run_systemctl(args: &[&str]) -> Result<()> {
        let status = std::process::Command::new("systemctl").args(args).status().context("spawn systemctl")?;
        if !status.success() {
            anyhow::bail!("systemctl {} exited {status}", args.join(" "));
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Windows — Task Scheduler via schtasks
// ---------------------------------------------------------------------------

#[cfg(target_os = "windows")]
mod windows {
    use super::{Context, OwoColorize, PathBuf, Result, LABEL};

    pub const TASK_NAME: &str = "SmoothAI";

    pub fn install_user(exe: &std::path::Path, daemon: bool) -> Result<()> {
        // Best-effort: delete an existing task so re-install is idempotent.
        let _ = std::process::Command::new("schtasks").args(["/Delete", "/TN", TASK_NAME, "/F"]).status();

        let cmd = if daemon {
            format!("\"{}\" daemon", exe.display())
        } else {
            format!("\"{}\" up --foreground", exe.display())
        };
        let out = std::process::Command::new("schtasks")
            .args(["/Create", "/SC", "ONLOGON", "/TN", TASK_NAME, "/TR", &cmd, "/RL", "LIMITED", "/F"])
            .output()
            .context("spawn schtasks")?;
        if !out.status.success() {
            anyhow::bail!("schtasks /Create failed: {}", String::from_utf8_lossy(&out.stderr).trim());
        }
        println!("\n  {} Scheduled task {TASK_NAME} registered (runs at logon).\n", "✓".green().bold());
        Ok(())
    }

    pub fn uninstall_user() -> Result<()> {
        let out = std::process::Command::new("schtasks")
            .args(["/Delete", "/TN", TASK_NAME, "/F"])
            .output()
            .context("spawn schtasks")?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            if stderr.contains("cannot find") {
                println!("\n  {} No scheduled task named {TASK_NAME}.\n", "ℹ".cyan());
                return Ok(());
            }
            anyhow::bail!("schtasks /Delete failed: {}", stderr.trim());
        }
        println!("\n  {} Removed scheduled task {TASK_NAME}.\n", "✓".green().bold());
        Ok(())
    }

    pub fn control(action: &str) -> Result<()> {
        match action {
            "start" => run(&["/Run", "/TN", TASK_NAME]),
            "stop" => run(&["/End", "/TN", TASK_NAME]),
            "restart" => {
                let _ = run(&["/End", "/TN", TASK_NAME]);
                run(&["/Run", "/TN", TASK_NAME])
            }
            "status" => run(&["/Query", "/TN", TASK_NAME, "/V", "/FO", "LIST"]),
            other => anyhow::bail!("unknown action: {other}"),
        }
    }

    fn run(args: &[&str]) -> Result<()> {
        let status = std::process::Command::new("schtasks").args(args).status().context("spawn schtasks")?;
        if !status.success() {
            anyhow::bail!("schtasks {} exited {status}", args.join(" "));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #[allow(unused_imports)]
    use super::*;

    #[test]
    fn parse_env_splits_and_validates() {
        let ok = parse_env(&["SMOOTH_ADDR=127.0.0.1:8788".to_string(), "X=a=b".to_string()]).unwrap();
        assert_eq!(
            ok,
            vec![("SMOOTH_ADDR".to_string(), "127.0.0.1:8788".to_string()), ("X".to_string(), "a=b".to_string())]
        );
        assert!(parse_env(&["noequals".to_string()]).is_err(), "missing = errors");
        assert!(parse_env(&["=v".to_string()]).is_err(), "empty key errors");
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn macos_plist_contains_required_keys() {
        let exe = std::path::PathBuf::from("/opt/th");
        let log = std::path::PathBuf::from("/tmp/smooth.log");
        let err = std::path::PathBuf::from("/tmp/smooth.err");
        let body = macos::render_plist(&exe, &log, &err, false, &[]);
        assert!(body.contains("<string>com.smooai.smooth</string>"));
        assert!(body.contains("<string>/opt/th</string>"));
        assert!(body.contains("<string>up</string>"));
        assert!(body.contains("<string>--foreground</string>"));
        // Daemon variant runs `th daemon` instead of `up --foreground`.
        let dbody = macos::render_plist(&exe, &log, &err, true, &[]);
        assert!(dbody.contains("<string>daemon</string>"));
        assert!(!dbody.contains("<string>up</string>"));
        assert!(body.contains("<key>KeepAlive</key>"));
        assert!(body.contains("<key>RunAtLoad</key>"));
        assert!(body.contains("<key>WorkingDirectory</key>"));
        assert!(body.contains("<key>ThrottleInterval</key>"));
        assert!(body.contains("/tmp/smooth.log"));
        assert!(body.contains("/tmp/smooth.err"));
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn macos_plist_bakes_extra_env() {
        let body = macos::render_plist(
            std::path::Path::new("/opt/th"),
            std::path::Path::new("/tmp/l"),
            std::path::Path::new("/tmp/e"),
            true,
            &[
                ("SMOOTH_ADDR".to_string(), "127.0.0.1:8788".to_string()),
                ("SMOOTH_TAILSCALE_HTTPS_PORT".to_string(), "8443".to_string()),
            ],
        );
        assert!(body.contains("<key>SMOOTH_ADDR</key>"));
        assert!(body.contains("<string>127.0.0.1:8788</string>"));
        assert!(body.contains("<key>SMOOTH_TAILSCALE_HTTPS_PORT</key>"));
        assert!(body.contains("<string>8443</string>"));
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn macos_updater_plist_runs_self_update_on_interval() {
        let body = macos::render_updater_plist(
            std::path::Path::new("/opt/th"),
            std::path::Path::new("/home/me/dev/smooai/smooth"),
            7200,
            std::path::Path::new("/tmp/u.log"),
            std::path::Path::new("/tmp/u.err"),
        );
        assert!(body.contains("<string>com.smooai.smooth.updater</string>"));
        assert!(body.contains("<string>self-update</string>"));
        assert!(body.contains("<string>/home/me/dev/smooai/smooth</string>"));
        assert!(body.contains("<key>StartInterval</key>"));
        assert!(body.contains("<integer>7200</integer>"));
        assert!(body.contains(".cargo/bin"), "PATH must include cargo bin for the build");
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn linux_unit_has_exec_start_and_install_section() {
        let exe = std::path::PathBuf::from("/opt/th");
        let log = std::path::PathBuf::from("/tmp/smooth.log");
        let err = std::path::PathBuf::from("/tmp/smooth.err");
        let unit = linux::render_unit(&exe, &log, &err, false, &[]);
        assert!(unit.contains("ExecStart=/opt/th up --foreground"));
        let dunit = linux::render_unit(&exe, &log, &err, true, &[("SMOOTH_ADDR".to_string(), "127.0.0.1:8788".to_string())]);
        assert!(dunit.contains("ExecStart=/opt/th daemon"));
        assert!(!dunit.contains("up --foreground"));
        assert!(dunit.contains("Environment=\"SMOOTH_ADDR=127.0.0.1:8788\""));
        assert!(unit.contains("WorkingDirectory=%h"));
        assert!(unit.contains("Restart=on-failure"));
        assert!(unit.contains("WantedBy=default.target"));
        assert!(unit.contains("append:/tmp/smooth.log"));
    }
}

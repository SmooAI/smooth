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

pub fn install(system: bool) -> Result<()> {
    let exe = std::env::current_exe().context("resolving current `th` executable path")?;
    let home = dirs_next::home_dir().context("cannot determine home directory")?;
    let log_path = home.join(".smooth").join("service.log");
    let err_path = home.join(".smooth").join("service.err");
    std::fs::create_dir_all(home.join(".smooth"))?;

    if system {
        print_system_artifact(&exe, &home, &log_path, &err_path);
        return Ok(());
    }

    #[cfg(target_os = "macos")]
    {
        macos::install_user(&exe, &log_path, &err_path)
    }
    #[cfg(target_os = "linux")]
    {
        linux::install_user(&exe, &log_path, &err_path, &home)
    }
    #[cfg(target_os = "windows")]
    {
        let _ = (log_path, err_path);
        windows::install_user(&exe)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        let _ = (exe, home, log_path, err_path);
        anyhow::bail!("th service is not implemented on this platform")
    }
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

fn print_system_artifact(exe: &std::path::Path, home: &std::path::Path, log: &std::path::Path, err: &std::path::Path) {
    println!("\n  {} System-level install prints the artifact; install it manually.\n", "ℹ".cyan());
    #[cfg(target_os = "macos")]
    {
        let plist = macos::render_plist(exe, log, err);
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
        let unit = linux::render_unit(exe, log, err);
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
    let _ = home;
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

    pub fn render_plist(exe: &std::path::Path, log: &std::path::Path, err: &std::path::Path) -> String {
        // Escape minimal XML-unsafe chars. Paths with & < > in them are
        // vanishingly rare on macOS but may as well be correct.
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
        <string>up</string>
        <string>--foreground</string>
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
    </dict>
</dict>
</plist>
"#,
            label = LABEL,
            exe = esc(&exe.display().to_string()),
            log = esc(&log.display().to_string()),
            err = esc(&err.display().to_string()),
            home = esc(&home),
        )
    }

    pub fn install_user(exe: &std::path::Path, log: &std::path::Path, err: &std::path::Path) -> Result<()> {
        let path = plist_path()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let body = render_plist(exe, log, err);
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

    pub fn uninstall_user() -> Result<()> {
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

    pub fn render_unit(exe: &std::path::Path, log: &std::path::Path, err: &std::path::Path) -> String {
        format!(
            r#"[Unit]
Description=Smooth (Smoo AI orchestration)
After=network.target

[Service]
Type=simple
ExecStart={exe} up --foreground
WorkingDirectory=%h
Restart=on-failure
RestartSec=3s
StandardOutput=append:{log}
StandardError=append:{err}

[Install]
WantedBy=default.target
"#,
            exe = exe.display(),
            log = log.display(),
            err = err.display(),
        )
    }

    pub fn install_user(exe: &std::path::Path, log: &std::path::Path, err: &std::path::Path, home: &std::path::Path) -> Result<()> {
        let path = unit_path(home);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, render_unit(exe, log, err)).with_context(|| format!("write {}", path.display()))?;

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

    pub fn install_user(exe: &std::path::Path) -> Result<()> {
        // Best-effort: delete an existing task so re-install is idempotent.
        let _ = std::process::Command::new("schtasks").args(["/Delete", "/TN", TASK_NAME, "/F"]).status();

        let cmd = format!("\"{}\" up --foreground", exe.display());
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
    #[cfg(target_os = "macos")]
    fn macos_plist_contains_required_keys() {
        let exe = std::path::PathBuf::from("/opt/th");
        let log = std::path::PathBuf::from("/tmp/smooth.log");
        let err = std::path::PathBuf::from("/tmp/smooth.err");
        let body = macos::render_plist(&exe, &log, &err);
        assert!(body.contains("<string>com.smooai.smooth</string>"));
        assert!(body.contains("<string>/opt/th</string>"));
        assert!(body.contains("<string>up</string>"));
        assert!(body.contains("<string>--foreground</string>"));
        assert!(body.contains("<key>KeepAlive</key>"));
        assert!(body.contains("<key>RunAtLoad</key>"));
        assert!(body.contains("<key>WorkingDirectory</key>"));
        assert!(body.contains("<key>ThrottleInterval</key>"));
        assert!(body.contains("/tmp/smooth.log"));
        assert!(body.contains("/tmp/smooth.err"));
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn linux_unit_has_exec_start_and_install_section() {
        let exe = std::path::PathBuf::from("/opt/th");
        let log = std::path::PathBuf::from("/tmp/smooth.log");
        let err = std::path::PathBuf::from("/tmp/smooth.err");
        let unit = linux::render_unit(&exe, &log, &err);
        assert!(unit.contains("ExecStart=/opt/th up --foreground"));
        assert!(unit.contains("WorkingDirectory=%h"));
        assert!(unit.contains("Restart=on-failure"));
        assert!(unit.contains("WantedBy=default.target"));
        assert!(unit.contains("append:/tmp/smooth.log"));
    }
}

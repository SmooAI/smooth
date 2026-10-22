//! `th doctor install` — a read-only inventory of every `th` and
//! `smooth-daemon` on this machine (SMOODEV-4114, phase 0 of "one daemon + one
//! th per machine").
//!
//! The audit behind it found five `th` copies (cargo, brew, two app bundles,
//! `~/.smooth/chrome/bin`), two daemons fighting for `~/.smooth`, a daemon that
//! dyld-aborted because a local build linked Homebrew's libssl, and Big Smooth
//! running daemon tools on an older brew `th`. Each of those was invisible
//! without a dozen manual `which -a` / `otool -L` / `lsof` calls; this prints
//! them in one place. It changes nothing.

use std::collections::BTreeSet;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::Result;
use owo_colors::OwoColorize as _;
use serde::Serialize;

const BINS: [&str; 2] = ["th", "smooth-daemon"];

/// One `th` / `smooth-daemon` binary found on disk.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Binary {
    pub name: String,
    pub path: PathBuf,
    /// Symlinks resolved; two entries with the same `real_path` are one binary.
    pub real_path: PathBuf,
    /// Where it was found: `PATH #1`, `Big Smooth.app`, `~/.smooth/bin`, ….
    pub source: String,
    pub version: Option<String>,
    /// Non-system dylibs (Homebrew prefixes) it links — it won't run on a Mac
    /// without them.
    pub foreign_dylibs: Vec<String>,
}

/// Which process holds one of the daemon's discovery files.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Holder {
    pub file: String,
    /// The advertised `host:port` (`daemon.addr` / `flow.addr`).
    pub addr: Option<String>,
    pub pid: Option<u32>,
    pub exe: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct StaleFile {
    pub path: PathBuf,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct Report {
    pub binaries: Vec<Binary>,
    pub holders: Vec<Holder>,
    pub stale: Vec<StaleFile>,
    pub warnings: Vec<String>,
}

pub fn run(json: bool) -> Result<()> {
    let home = dirs_next::home_dir().unwrap_or_else(|| PathBuf::from("."));
    let path_var = std::env::var("PATH").unwrap_or_default();
    let report = collect(&home, &candidates(&home, &path_var, &system_dirs()), &SystemProbe);
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print_report(&report);
    }
    Ok(())
}

/// The side-effecting reads, behind a trait so [`collect`] is testable.
pub trait Probe {
    fn version(&self, bin: &Path) -> Option<String>;
    /// Raw `otool -L` / `ldd` output for `bin`.
    fn linked_libs(&self, bin: &Path) -> Option<String>;
    fn lock_holders(&self, file: &Path) -> Vec<u32>;
    fn port_listeners(&self, port: u16) -> Vec<u32>;
    fn exe_of(&self, pid: u32) -> Option<String>;
    fn alive(&self, pid: u32) -> bool;
}

pub fn collect(home: &Path, candidates: &[(String, PathBuf)], probe: &dyn Probe) -> Report {
    let smooth = home.join(".smooth");
    let mut report = Report::default();

    for (source, path) in candidates.iter().cloned() {
        let Some(name) = path.file_name().and_then(|n| n.to_str()).map(str::to_owned) else {
            continue;
        };
        if !path.is_file() {
            continue;
        }
        let real_path = std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
        let foreign_dylibs = probe.linked_libs(&real_path).map(|out| foreign_dylibs(&out)).unwrap_or_default();
        report.binaries.push(Binary {
            name,
            version: probe.version(&real_path),
            path,
            real_path,
            source,
            foreign_dylibs,
        });
    }

    report.holders = holders(&smooth, probe);
    report.stale = stale_files(&smooth, &home.join("Library").join("LaunchAgents"), probe);
    report.warnings = warnings(&report);
    report
}

/// Machine-wide installs outside `$HOME` that are not normally on PATH.
fn system_dirs() -> Vec<(&'static str, PathBuf)> {
    vec![
        ("Big Smooth.app", PathBuf::from("/Applications/Big Smooth.app/Contents/Resources")),
        ("SmoothFlow.app", PathBuf::from("/Applications/SmoothFlow.app/Contents/MacOS")),
        ("Homebrew", PathBuf::from("/opt/homebrew/bin")),
    ]
}

/// Every place a `th` / `smooth-daemon` lives: each PATH dir (in PATH order),
/// then `system` installs, then the per-user ones under `home`.
pub fn candidates(home: &Path, path_var: &str, system: &[(&str, PathBuf)]) -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    for (i, dir) in std::env::split_paths(path_var).enumerate() {
        if dir.as_os_str().is_empty() {
            continue;
        }
        for bin in BINS {
            let p = dir.join(exe(bin));
            if seen.insert(p.clone()) {
                out.push((format!("PATH #{}", i + 1), p));
            }
        }
    }
    let user = [
        ("~/.smooth/bin", home.join(".smooth").join("bin")),
        ("~/.smooth/chrome/bin", home.join(".smooth").join("chrome").join("bin")),
        ("~/.cargo/bin", home.join(".cargo").join("bin")),
    ];
    for (source, dir) in system.iter().cloned().chain(user) {
        for bin in BINS {
            let p = dir.join(exe(bin));
            if seen.insert(p.clone()) {
                out.push((source.to_owned(), p));
            }
        }
    }
    out
}

fn exe(bin: &str) -> String {
    if cfg!(windows) {
        format!("{bin}.exe")
    } else {
        bin.to_owned()
    }
}

/// Library paths in `otool -L` / `ldd` output that live under a package
/// manager prefix rather than the OS — they exist on the build machine, not on
/// the user's.
pub fn foreign_dylibs(linker_output: &str) -> Vec<String> {
    const PREFIXES: [&str; 3] = ["/opt/homebrew/", "/usr/local/", "/home/linuxbrew/"];
    linker_output
        .lines()
        // otool's first line is the binary itself, ending in `:`.
        .filter(|l| !l.trim_end().ends_with(':'))
        .filter_map(|l| {
            // otool: `\t/path (compatibility …)`; ldd: `\tname => /path (0x…)`.
            let l = l.split("=>").last().unwrap_or(l).trim();
            let path = l.split(" (").next().unwrap_or(l).trim();
            PREFIXES.iter().any(|p| path.starts_with(p)).then(|| path.to_owned())
        })
        .collect()
}

/// The first `major.minor.patch` in `text` (`th 0.84.1 (3719cc9)` → `0.84.1`).
pub fn parse_version(text: &str) -> Option<String> {
    text.split(|c: char| !(c.is_ascii_digit() || c == '.'))
        .find(|w| {
            let parts: Vec<_> = w.split('.').collect();
            parts.len() == 3 && parts.iter().all(|p| !p.is_empty())
        })
        .map(str::to_owned)
}

fn version_key(v: &str) -> (u64, u64, u64) {
    let mut it = v.split('.').map(|p| p.parse::<u64>().unwrap_or(0));
    (it.next().unwrap_or(0), it.next().unwrap_or(0), it.next().unwrap_or(0))
}

/// The port of a `host:port` address.
pub fn parse_port(addr: &str) -> Option<u16> {
    addr.trim().rsplit_once(':')?.1.parse().ok()
}

fn holders(smooth: &Path, probe: &dyn Probe) -> Vec<Holder> {
    let mut out = Vec::new();
    let lock = smooth.join("daemon.lock");
    if lock.exists() {
        let pid = probe.lock_holders(&lock).into_iter().next();
        out.push(Holder {
            file: "daemon.lock".into(),
            addr: None,
            pid,
            exe: pid.and_then(|p| probe.exe_of(p)),
        });
    }
    for file in ["daemon.addr", "flow.addr"] {
        let Ok(raw) = std::fs::read_to_string(smooth.join(file)) else {
            continue;
        };
        let addr = raw.trim().to_owned();
        let pid = parse_port(&addr).and_then(|port| probe.port_listeners(port).into_iter().next());
        out.push(Holder {
            file: file.into(),
            addr: (!addr.is_empty()).then_some(addr),
            pid,
            exe: pid.and_then(|p| probe.exe_of(p)),
        });
    }
    out
}

/// Leftovers that confuse "which daemon is running": a pid file whose process
/// is gone, `*.bak` copies under `~/.smooth`, and a backed-up launchd plist.
pub fn stale_files(smooth: &Path, launch_agents: &Path, probe: &dyn Probe) -> Vec<StaleFile> {
    let mut out = Vec::new();
    for pid_file in ["smooth.pid", "smoothflow-daemon.pid"] {
        let path = smooth.join(pid_file);
        let Ok(raw) = std::fs::read_to_string(&path) else {
            continue;
        };
        match raw.trim().parse::<u32>() {
            Ok(pid) if probe.alive(pid) => {}
            Ok(pid) => out.push(StaleFile {
                path,
                reason: format!("pid {pid} is not running"),
            }),
            Err(_) => out.push(StaleFile {
                path,
                reason: "not a pid".into(),
            }),
        }
    }
    let mut baks: Vec<PathBuf> = std::fs::read_dir(smooth)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.contains(".bak")))
        .collect();
    baks.sort();
    out.extend(baks.into_iter().map(|path| StaleFile {
        path,
        reason: "backup copy".into(),
    }));
    let plist = launch_agents.join("com.smooai.smooth.plist.bak");
    if plist.exists() {
        out.push(StaleFile {
            path: plist,
            reason: "backed-up launchd unit".into(),
        });
    }
    out
}

pub fn warnings(report: &Report) -> Vec<String> {
    let mut out = Vec::new();
    for name in BINS {
        let mut seen = BTreeSet::new();
        let found: Vec<&Binary> = report.binaries.iter().filter(|b| b.name == name && seen.insert(b.real_path.clone())).collect();
        let versions: BTreeSet<&str> = found.iter().filter_map(|b| b.version.as_deref()).collect();
        if versions.len() > 1 {
            let newest = versions.iter().max_by_key(|v| version_key(v)).copied().unwrap_or_default();
            let older: Vec<String> = found
                .iter()
                .filter(|b| b.version.as_deref().is_some_and(|v| version_key(v) < version_key(newest)))
                .map(|b| format!("{} {} ({})", b.path.display(), b.version.as_deref().unwrap_or("?"), b.source))
                .collect();
            out.push(format!("{name} version skew: newest is {newest}; older: {}", older.join(", ")));
        }
        if let Some(first) = found.first().filter(|b| b.source.starts_with("PATH")) {
            if let (Some(v), Some(newest)) = (&first.version, versions.iter().max_by_key(|v| version_key(v))) {
                if version_key(v) < version_key(newest) {
                    out.push(format!(
                        "`{name}` on PATH resolves to {} ({v}), not the newest ({newest})",
                        first.path.display()
                    ));
                }
            }
        }
    }
    for b in &report.binaries {
        if !b.foreign_dylibs.is_empty() {
            out.push(format!(
                "{} links non-system libraries ({}) — it will not run on a machine without them",
                b.path.display(),
                b.foreign_dylibs.join(", ")
            ));
        }
    }
    let pids: BTreeSet<u32> = report.holders.iter().filter_map(|h| h.pid).collect();
    if pids.len() > 1 {
        let who: Vec<String> = report.holders.iter().filter_map(|h| h.pid.map(|p| format!("{} → pid {p}", h.file))).collect();
        out.push(format!("more than one daemon owns the discovery files: {}", who.join(", ")));
    }
    for h in &report.holders {
        if h.file != "daemon.lock" && h.addr.is_some() && h.pid.is_none() {
            out.push(format!(
                "{} advertises {} but nothing is listening there",
                h.file,
                h.addr.as_deref().unwrap_or("")
            ));
        }
    }
    out
}

fn print_report(r: &Report) {
    println!("{}", "th / smooth-daemon installs".bold());
    if r.binaries.is_empty() {
        println!("  {} no th or smooth-daemon found on PATH or in the known install locations", "○".dimmed());
    }
    let mut seen = BTreeSet::new();
    for b in &r.binaries {
        let dup = !seen.insert(b.real_path.clone());
        let link = if b.real_path == b.path {
            String::new()
        } else {
            format!(" → {}", b.real_path.display())
        };
        let glyph = if b.foreign_dylibs.is_empty() {
            "●".green().to_string()
        } else {
            "✗".red().to_string()
        };
        println!(
            "  {glyph} {:<13} {:<9} {}{}  {}{}",
            b.name,
            b.version.as_deref().unwrap_or("?"),
            b.path.display(),
            link.dimmed(),
            b.source.dimmed(),
            if dup { " (same binary as above)".dimmed().to_string() } else { String::new() }
        );
    }
    println!();
    println!("{}", "daemon discovery files (~/.smooth)".bold());
    if r.holders.is_empty() {
        println!("  {} none — no daemon has run here, or it was cleaned up", "○".dimmed());
    }
    for h in &r.holders {
        let who = match (h.pid, &h.exe) {
            (Some(pid), Some(exe)) => format!("pid {pid} {exe}"),
            (Some(pid), None) => format!("pid {pid}"),
            (None, _) => "no live holder".dimmed().to_string(),
        };
        let addr = h.addr.as_deref().map(|a| format!(" {a}")).unwrap_or_default();
        println!("  {} {}{addr}: {who}", if h.pid.is_some() { "●" } else { "○" }, h.file);
    }
    println!();
    println!("{}", "stale files".bold());
    if r.stale.is_empty() {
        println!("  {} none", "○".dimmed());
    }
    for s in &r.stale {
        println!("  ◐ {} ({})", s.path.display(), s.reason);
    }
    println!();
    if r.warnings.is_empty() {
        println!("{} no version skew, foreign dylibs or competing daemons", "✓".green().bold());
    } else {
        for w in &r.warnings {
            println!("{} {w}", "✗".red().bold());
        }
    }
    eprintln!("{}", "Read-only: nothing was changed.".dimmed());
}

/// The real probes: `--version`, `otool -L`/`ldd`, `lsof`, `ps`. Every call
/// is bounded — a first run of a freshly downloaded binary can stall while
/// macOS validates it.
struct SystemProbe;

const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

fn output_with_timeout(cmd: &mut Command) -> Option<String> {
    let mut child = cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().ok()?;
    let mut stdout = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = stdout.read_to_string(&mut s);
        s
    });
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if start.elapsed() < PROBE_TIMEOUT => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    reader.join().ok()
}

fn pids(text: &str) -> Vec<u32> {
    text.lines().filter_map(|l| l.trim().parse().ok()).collect()
}

impl Probe for SystemProbe {
    fn version(&self, bin: &Path) -> Option<String> {
        parse_version(&output_with_timeout(Command::new(bin).arg("--version"))?)
    }

    fn linked_libs(&self, bin: &Path) -> Option<String> {
        if cfg!(target_os = "macos") {
            output_with_timeout(Command::new("otool").arg("-L").arg(bin))
        } else if cfg!(target_os = "linux") {
            output_with_timeout(Command::new("ldd").arg(bin))
        } else {
            None
        }
    }

    fn lock_holders(&self, file: &Path) -> Vec<u32> {
        output_with_timeout(Command::new("lsof").arg("-t").arg(file))
            .map(|o| pids(&o))
            .unwrap_or_default()
    }

    fn port_listeners(&self, port: u16) -> Vec<u32> {
        output_with_timeout(Command::new("lsof").args(["-nP", "-t", &format!("-iTCP:{port}"), "-sTCP:LISTEN"]))
            .map(|o| pids(&o))
            .unwrap_or_default()
    }

    fn exe_of(&self, pid: u32) -> Option<String> {
        let out = output_with_timeout(Command::new("ps").args(["-o", "comm=", "-p", &pid.to_string()]))?;
        let out = out.trim();
        (!out.is_empty()).then(|| out.to_owned())
    }

    fn alive(&self, pid: u32) -> bool {
        self.exe_of(pid).is_some()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    #[derive(Default)]
    struct FakeProbe {
        versions: HashMap<PathBuf, String>,
        libs: HashMap<PathBuf, String>,
        lock: Vec<u32>,
        ports: HashMap<u16, u32>,
        exes: HashMap<u32, String>,
    }

    impl Probe for FakeProbe {
        fn version(&self, bin: &Path) -> Option<String> {
            self.versions.get(bin).cloned()
        }
        fn linked_libs(&self, bin: &Path) -> Option<String> {
            self.libs.get(bin).cloned()
        }
        fn lock_holders(&self, _: &Path) -> Vec<u32> {
            self.lock.clone()
        }
        fn port_listeners(&self, port: u16) -> Vec<u32> {
            self.ports.get(&port).into_iter().copied().collect()
        }
        fn exe_of(&self, pid: u32) -> Option<String> {
            self.exes.get(&pid).cloned()
        }
        fn alive(&self, pid: u32) -> bool {
            self.exes.contains_key(&pid)
        }
    }

    fn touch(p: &Path) -> PathBuf {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, "x").unwrap();
        std::fs::canonicalize(p).unwrap()
    }

    #[test]
    fn foreign_dylibs_reads_otool_and_ldd() {
        let otool = "/x/smooth-daemon:\n\t/opt/homebrew/opt/openssl@3/lib/libssl.3.dylib (compatibility version 3.0.0)\n\t/usr/lib/libSystem.B.dylib (compatibility version 1.0.0)\n\t/System/Library/Frameworks/Security.framework/Versions/A/Security (compatibility version 1.0.0)\n";
        assert_eq!(foreign_dylibs(otool), vec!["/opt/homebrew/opt/openssl@3/lib/libssl.3.dylib"]);
        let ldd = "\tlinux-vdso.so.1 (0x00007ffd)\n\tlibssl.so.3 => /home/linuxbrew/.linuxbrew/lib/libssl.so.3 (0x00007f)\n\tlibc.so.6 => /lib/x86_64-linux-gnu/libc.so.6 (0x00007f)\n";
        assert_eq!(foreign_dylibs(ldd), vec!["/home/linuxbrew/.linuxbrew/lib/libssl.so.3"]);
        assert!(
            foreign_dylibs("/usr/local/bin/th:\n\t/usr/lib/libSystem.B.dylib (compatibility version 1.0.0)\n").is_empty(),
            "the binary's own path is not a dylib"
        );
    }

    #[test]
    fn parse_version_and_port() {
        assert_eq!(parse_version("th 0.84.1 (3719cc9)").as_deref(), Some("0.84.1"));
        assert_eq!(parse_version("smooth-daemon 0.81.1\n").as_deref(), Some("0.81.1"));
        assert_eq!(parse_version("no version here 1.2"), None);
        assert_eq!(parse_port("127.0.0.1:57043\n"), Some(57043));
        assert_eq!(parse_port("[::1]:8787"), Some(8787));
        assert_eq!(parse_port("garbage"), None);
        assert!(version_key("0.84.1") > version_key("0.81.10"));
        assert!(version_key("0.100.0") > version_key("0.99.9"), "numeric, not lexical");
    }

    #[test]
    fn candidates_walk_path_then_known_installs_without_duplicates() {
        let home = Path::new("/h");
        let path = std::env::join_paths(["/a", "/b", "/a"]).unwrap();
        let c = candidates(home, path.to_str().unwrap(), &system_dirs());
        let paths: Vec<_> = c.iter().map(|(_, p)| p.clone()).collect();
        assert_eq!(paths[0], Path::new("/a").join(exe("th")));
        assert_eq!(c[0].0, "PATH #1");
        assert_eq!(c[2].0, "PATH #2");
        assert_eq!(paths.iter().filter(|p| **p == Path::new("/a").join(exe("th"))).count(), 1, "deduped");
        assert!(c
            .iter()
            .any(|(s, p)| s == "~/.smooth/chrome/bin" && *p == Path::new("/h/.smooth/chrome/bin").join(exe("th"))));
        assert!(c.iter().any(|(s, _)| s == "Big Smooth.app"));
        assert!(c.iter().any(|(s, _)| s == "SmoothFlow.app"));
    }

    #[test]
    fn collect_reports_skew_dylibs_holders_and_stale_files() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let brew_th = touch(&home.join("brew/th"));
        let cargo_th = touch(&home.join(".cargo/bin/th"));
        let daemon = touch(&home.join(".cargo/bin/smooth-daemon"));
        let smooth = home.join(".smooth");
        std::fs::create_dir_all(&smooth).unwrap();
        std::fs::write(smooth.join("daemon.lock"), "").unwrap();
        std::fs::write(smooth.join("daemon.addr"), "127.0.0.1:4400\n").unwrap();
        std::fs::write(smooth.join("flow.addr"), "127.0.0.1:4500\n").unwrap();
        std::fs::write(smooth.join("smooth.pid"), "99999").unwrap();
        std::fs::write(smooth.join("daemon.addr.bak"), "old").unwrap();
        touch(&home.join("Library/LaunchAgents/com.smooai.smooth.plist.bak"));

        let probe = FakeProbe {
            versions: HashMap::from([
                (brew_th.clone(), "0.81.1".into()),
                (cargo_th.clone(), "0.84.1".into()),
                (daemon.clone(), "0.84.1".into()),
            ]),
            libs: HashMap::from([(daemon.clone(), "x:\n\t/opt/homebrew/opt/openssl@3/lib/libssl.3.dylib (c)\n".into())]),
            lock: vec![10],
            ports: HashMap::from([(4400, 10), (4500, 20)]),
            exes: HashMap::from([
                (10, "/Applications/Big Smooth.app/Contents/Resources/smooth-daemon".into()),
                (20, "/x/smooth-daemon".into()),
            ]),
        };
        let path_var = std::env::join_paths([home.join("brew")]).unwrap();
        let r = collect(home, &candidates(home, path_var.to_str().unwrap(), &[]), &probe);

        let th: Vec<_> = r.binaries.iter().filter(|b| b.name == "th").collect();
        assert_eq!(th.len(), 2);
        assert_eq!(th[0].source, "PATH #1");
        assert_eq!(th[1].source, "~/.cargo/bin");

        assert_eq!(r.holders.len(), 3);
        assert_eq!(r.holders[0].pid, Some(10));
        assert_eq!(r.holders[2].addr.as_deref(), Some("127.0.0.1:4500"));
        assert_eq!(r.holders[2].exe.as_deref(), Some("/x/smooth-daemon"));

        let stale: Vec<_> = r.stale.iter().map(|s| s.path.file_name().unwrap().to_str().unwrap().to_owned()).collect();
        assert_eq!(stale, vec!["smooth.pid", "daemon.addr.bak", "com.smooai.smooth.plist.bak"]);

        let w = r.warnings.join("\n");
        assert!(w.contains("th version skew: newest is 0.84.1"), "{w}");
        assert!(w.contains("on PATH resolves to"), "{w}");
        assert!(w.contains("libssl.3.dylib"), "{w}");
        assert!(w.contains("more than one daemon owns the discovery files"), "{w}");
    }

    #[test]
    fn healthy_machine_has_no_warnings() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let th = touch(&home.join(".smooth/bin/th"));
        let smooth = home.join(".smooth");
        std::fs::write(smooth.join("daemon.addr"), "127.0.0.1:4400").unwrap();
        std::fs::write(smooth.join("smooth.pid"), "10").unwrap();
        let probe = FakeProbe {
            versions: HashMap::from([(th, "0.84.1".into())]),
            ports: HashMap::from([(4400, 10)]),
            exes: HashMap::from([(10, "smooth-daemon".into())]),
            ..FakeProbe::default()
        };
        let r = collect(home, &candidates(home, "", &[]), &probe);
        assert_eq!(r.binaries.len(), 1);
        assert!(r.stale.is_empty(), "live pid file is not stale: {:?}", r.stale);
        assert!(r.warnings.is_empty(), "{:?}", r.warnings);
    }

    #[test]
    fn dead_advertised_addr_is_flagged() {
        let tmp = tempfile::tempdir().unwrap();
        let smooth = tmp.path().join(".smooth");
        std::fs::create_dir_all(&smooth).unwrap();
        std::fs::write(smooth.join("daemon.addr"), "127.0.0.1:4400").unwrap();
        std::fs::write(smooth.join("smooth.pid"), "nope").unwrap();
        let r = collect(tmp.path(), &candidates(tmp.path(), "", &[]), &FakeProbe::default());
        assert!(r.warnings.iter().any(|w| w.contains("nothing is listening")), "{:?}", r.warnings);
        assert_eq!(r.stale[0].reason, "not a pid");
    }

    #[test]
    fn same_binary_through_a_symlink_is_not_skew() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let real = touch(&home.join("app/th"));
        std::fs::create_dir_all(home.join("bin")).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&real, home.join("bin/th")).unwrap();
        let probe = FakeProbe {
            versions: HashMap::from([(real.clone(), "0.84.1".into())]),
            ..FakeProbe::default()
        };
        let path_var = std::env::join_paths([home.join("bin"), home.join("app")]).unwrap();
        let r = collect(home, &candidates(home, path_var.to_str().unwrap(), &[]), &probe);
        assert!(r.binaries.iter().all(|b| b.real_path == real));
        assert!(r.warnings.is_empty(), "{:?}", r.warnings);
    }

    #[test]
    fn output_with_timeout_reads_and_bounds() {
        #[cfg(unix)]
        {
            assert_eq!(output_with_timeout(Command::new("echo").arg("th 1.2.3")).as_deref(), Some("th 1.2.3\n"));
            assert!(output_with_timeout(&mut Command::new("/definitely/not/a/binary")).is_none());
        }
    }
}

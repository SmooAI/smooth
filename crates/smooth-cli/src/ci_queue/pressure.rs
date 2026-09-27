//! Machine-pressure readings and the gate that turns them into "hold".
//!
//! Readings come through the [`Probe`] trait so the queue's tests inject them;
//! nothing in a test reads the real machine. [`SystemProbe`] is the real one:
//! `vm_stat` + `sysctl` on macOS, `/proc` on Linux, `df -Pk` for disks.
//!
//! An unreadable signal is `None` and never holds a job. The gate is a
//! throttle on top of the slot cap, not a safety interlock, so "don't know"
//! falls back to the slot cap rather than stalling every commit on a probe.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde::Serialize;

use super::config::Gate;

const GIB: f64 = 1024.0 * 1024.0 * 1024.0;

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Readings {
    pub mem_total_bytes: Option<u64>,
    pub mem_available_bytes: Option<u64>,
    /// macOS `kern.memorystatus_vm_pressure_level`: 1 normal, 2 warn, 4 critical.
    pub memory_pressure_level: Option<u32>,
    pub swap_total_bytes: Option<u64>,
    pub swap_used_bytes: Option<u64>,
    pub load1: Option<f64>,
    pub cores: usize,
    pub disks: Vec<Disk>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Disk {
    pub path: PathBuf,
    pub free_bytes: Option<u64>,
}

impl Readings {
    #[allow(clippy::cast_precision_loss, reason = "a percentage of RAM does not need 53 bits")]
    pub fn mem_available_pct(&self) -> Option<f64> {
        match (self.mem_available_bytes, self.mem_total_bytes) {
            (Some(a), Some(t)) if t > 0 => Some(a as f64 * 100.0 / t as f64),
            _ => None,
        }
    }

    /// No swap configured reads as 0% used, not unknown.
    #[allow(clippy::cast_precision_loss, reason = "a percentage of swap does not need 53 bits")]
    pub fn swap_used_pct(&self) -> Option<f64> {
        match (self.swap_used_bytes, self.swap_total_bytes) {
            (Some(_), Some(0)) => Some(0.0),
            (Some(u), Some(t)) => Some(u as f64 * 100.0 / t as f64),
            _ => None,
        }
    }

    #[allow(clippy::cast_precision_loss, reason = "core counts are tiny")]
    pub fn load_per_core(&self) -> Option<f64> {
        self.load1.filter(|_| self.cores > 0).map(|l| l / self.cores as f64)
    }
}

/// Where readings come from. `Send + Sync` so a `Queue` can be shared.
pub trait Probe: Send + Sync {
    fn read(&self, disk_paths: &[PathBuf]) -> Readings;
}

/// Why the gate is holding new heavy jobs right now; empty means it is not.
/// Each entry is one short phrase for the `waiting (…)` line.
pub fn holds(r: &Readings, g: &Gate) -> Vec<String> {
    let mut why = Vec::new();
    let avail = r.mem_available_pct();

    if g.min_available_memory_pct > 0.0 {
        if let Some(a) = avail.filter(|a| *a < g.min_available_memory_pct) {
            why.push(format!("memory {a:.0}% available"));
        }
    }
    let pressured = g.max_memory_pressure_level > 0 && r.memory_pressure_level.is_some_and(|l| l > g.max_memory_pressure_level);
    if pressured {
        why.push(format!("memory pressure {}", pressure_name(r.memory_pressure_level.unwrap_or_default())));
    }
    if g.max_swap_used_pct > 0.0 {
        if let Some(s) = r.swap_used_pct().filter(|s| *s > g.max_swap_used_pct) {
            // Swap is sticky on macOS; it only means something alongside memory
            // that is tight now. See `Gate::max_swap_used_pct`.
            let tight_level = r.memory_pressure_level.is_some_and(|l| l > 1);
            let tight_avail = g.min_available_memory_pct > 0.0 && avail.is_some_and(|a| a < 2.0 * g.min_available_memory_pct);
            if tight_level || tight_avail {
                why.push(format!("swap {s:.0}%"));
            }
        }
    }
    if g.max_load_per_core > 0.0 {
        if let (Some(per), Some(l)) = (r.load_per_core(), r.load1) {
            if per > g.max_load_per_core {
                why.push(format!("load {l:.1} on {} cores", r.cores));
            }
        }
    }
    if g.min_free_disk_gb > 0.0 {
        for d in &r.disks {
            if let Some(free) = d.free_bytes {
                #[allow(clippy::cast_precision_loss, reason = "GB display")]
                let gb = free as f64 / GIB;
                if gb < g.min_free_disk_gb {
                    why.push(format!("disk {gb:.1} GB free at {}", d.path.display()));
                }
            }
        }
    }
    why
}

pub const fn pressure_name(level: u32) -> &'static str {
    match level {
        0 | 1 => "normal",
        2 => "warn",
        _ => "critical",
    }
}

/// The real machine.
pub struct SystemProbe;

impl Probe for SystemProbe {
    fn read(&self, disk_paths: &[PathBuf]) -> Readings {
        let mut r = Readings {
            cores: std::thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get),
            disks: disk_paths
                .iter()
                .map(|p| Disk {
                    path: p.clone(),
                    free_bytes: disk_free(p),
                })
                .collect(),
            ..Readings::default()
        };
        if cfg!(target_os = "macos") {
            r.mem_total_bytes = sysctl("hw.memsize").and_then(|s| s.parse().ok());
            r.mem_available_bytes = output("vm_stat", &[]).and_then(|s| parse_vm_stat(&s));
            r.memory_pressure_level = sysctl("kern.memorystatus_vm_pressure_level").and_then(|s| s.parse().ok());
            if let Some((total, used)) = sysctl("vm.swapusage").and_then(|s| parse_swapusage(&s)) {
                r.swap_total_bytes = Some(total);
                r.swap_used_bytes = Some(used);
            }
            r.load1 = sysctl("vm.loadavg").and_then(|s| parse_loadavg(&s));
        } else if let Ok(meminfo) = std::fs::read_to_string("/proc/meminfo") {
            let m = parse_meminfo(&meminfo);
            r.mem_total_bytes = m.total;
            r.mem_available_bytes = m.available;
            r.swap_total_bytes = m.swap_total;
            r.swap_used_bytes = m.swap_total.zip(m.swap_free).map(|(t, f)| t.saturating_sub(f));
            r.load1 = std::fs::read_to_string("/proc/loadavg").ok().and_then(|s| parse_loadavg(&s));
        }
        r
    }
}

fn output(program: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(program).args(args).stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn sysctl(key: &str) -> Option<String> {
    output("sysctl", &["-n", key])
}

/// Free bytes on the volume holding `path`, via `df -Pk` (POSIX output, 1K
/// blocks — identical columns on macOS and Linux). A path that does not exist
/// yet is measured at its nearest existing ancestor.
fn disk_free(path: &Path) -> Option<u64> {
    let existing = path.ancestors().find(|p| p.exists())?;
    let out = Command::new("df")
        .arg("-Pk")
        .arg(existing)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    parse_df(&String::from_utf8_lossy(&out.stdout))
}

pub fn parse_df(out: &str) -> Option<u64> {
    let line = out.lines().filter(|l| !l.trim().is_empty()).nth(1)?;
    // Filesystem 1024-blocks Used Available Capacity Mounted-on. The
    // filesystem name can contain spaces (macOS "map auto_home"), so count
    // from the right: Available is the 4th-from-last field... except the mount
    // point can contain spaces too. The capacity column is the one ending in
    // `%`; Available is the field just before it.
    let fields: Vec<&str> = line.split_whitespace().collect();
    let pct = fields.iter().position(|f| f.ends_with('%'))?;
    fields.get(pct.checked_sub(1)?)?.parse::<u64>().ok().map(|k| k * 1024)
}

/// macOS `vm_stat`: available = (free + inactive + speculative + purgeable)
/// pages, at the page size its header states.
pub fn parse_vm_stat(out: &str) -> Option<u64> {
    let page: u64 = out
        .lines()
        .next()
        .and_then(|l| l.split("page size of ").nth(1))
        .and_then(|s| s.split_whitespace().next())
        .and_then(|s| s.parse().ok())?;
    let pages = |key: &str| -> Option<u64> {
        out.lines()
            .find(|l| l.trim_start().starts_with(key))
            .and_then(|l| l.rsplit(':').next())
            .and_then(|v| v.trim().trim_end_matches('.').parse().ok())
    };
    let free = pages("Pages free")?;
    let sum = free + pages("Pages inactive").unwrap_or(0) + pages("Pages speculative").unwrap_or(0) + pages("Pages purgeable").unwrap_or(0);
    Some(sum * page)
}

/// macOS `sysctl -n vm.swapusage`: `total = 19456.00M  used = 18125.12M  free = …`.
pub fn parse_swapusage(out: &str) -> Option<(u64, u64)> {
    let field = |key: &str| -> Option<u64> {
        let rest = out.split(&format!("{key} = ")).nth(1)?;
        let tok = rest.split_whitespace().next()?;
        let (num, unit) = tok.split_at(tok.find(|c: char| c.is_ascii_alphabetic()).unwrap_or(tok.len()));
        let n: f64 = num.parse().ok()?;
        let mult = match unit {
            "K" => 1024.0,
            "M" => 1024.0 * 1024.0,
            "G" => GIB,
            "" | "B" => 1.0,
            _ => return None,
        };
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "non-negative byte count")]
        Some((n * mult) as u64)
    };
    Some((field("total")?, field("used")?))
}

/// `{ 37.36 43.35 38.83 }` (macOS `vm.loadavg`) or `37.36 43.35 …` (Linux
/// `/proc/loadavg`) → the 1-minute figure.
pub fn parse_loadavg(out: &str) -> Option<f64> {
    out.split(|c: char| c.is_whitespace() || c == '{' || c == '}')
        .find(|t| !t.is_empty())
        .and_then(|t| t.parse().ok())
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct MemInfo {
    pub total: Option<u64>,
    pub available: Option<u64>,
    pub swap_total: Option<u64>,
    pub swap_free: Option<u64>,
}

/// Linux `/proc/meminfo`, in bytes.
pub fn parse_meminfo(out: &str) -> MemInfo {
    let kb = |key: &str| -> Option<u64> {
        out.lines()
            .find(|l| l.starts_with(key) && l[key.len()..].starts_with(':'))
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|v| v.parse::<u64>().ok())
            .map(|k| k * 1024)
    };
    MemInfo {
        total: kb("MemTotal"),
        available: kb("MemAvailable"),
        swap_total: kb("SwapTotal"),
        swap_free: kb("SwapFree"),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "unwrap is the idiom for test assertions")]
mod tests {
    use super::*;

    const VM_STAT: &str = "Mach Virtual Memory Statistics: (page size of 16384 bytes)
Pages free:                              319606.
Pages active:                           1304422.
Pages inactive:                         1124126.
Pages speculative:                       178698.
Pages throttled:                              0.
Pages wired down:                        366407.
Pages purgeable:                          15837.
\"Translation faults\":               10533473753.
";

    fn gib(n: f64) -> u64 {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let b = (n * GIB) as u64;
        b
    }

    /// A healthy 64 GB, 12-core machine — the shape of the incident box on a
    /// quiet day, including swap that is nearly full and means nothing.
    fn calm() -> Readings {
        Readings {
            mem_total_bytes: Some(gib(64.0)),
            mem_available_bytes: Some(gib(26.0)),
            memory_pressure_level: Some(1),
            swap_total_bytes: Some(gib(19.0)),
            swap_used_bytes: Some(gib(17.7)),
            load1: Some(30.0),
            cores: 12,
            disks: vec![Disk {
                path: "/work".into(),
                free_bytes: Some(gib(160.0)),
            }],
        }
    }

    #[test]
    fn parses_vm_stat_available_bytes() {
        let pages = 319_606 + 1_124_126 + 178_698 + 15_837;
        assert_eq!(parse_vm_stat(VM_STAT), Some(pages * 16384));
    }

    #[test]
    fn vm_stat_without_a_page_size_is_unknown() {
        assert_eq!(parse_vm_stat("Pages free: 12.\n"), None);
    }

    #[test]
    #[allow(clippy::cast_precision_loss, reason = "test tolerance")]
    fn parses_macos_swapusage() {
        let (t, u) = parse_swapusage("total = 19456.00M  used = 18125.12M  free = 1330.88M  (encrypted)").unwrap();
        assert_eq!(t, 19456 * 1024 * 1024);
        assert!((u as f64 / (1024.0 * 1024.0) - 18125.12).abs() < 0.01);
        assert_eq!(parse_swapusage("total = 0.00M  used = 0.00M  free = 0.00M"), Some((0, 0)));
    }

    #[test]
    fn parses_both_loadavg_shapes() {
        assert_eq!(parse_loadavg("{ 37.36 43.35 38.83 }"), Some(37.36));
        assert_eq!(parse_loadavg("1.50 0.75 0.20 2/345 6789\n"), Some(1.5));
        assert_eq!(parse_loadavg(""), None);
    }

    #[test]
    fn parses_linux_meminfo() {
        let m = parse_meminfo("MemTotal:       16000000 kB\nMemFree: 1 kB\nMemAvailable:    4000000 kB\nSwapTotal: 2000 kB\nSwapFree: 500 kB\n");
        assert_eq!(
            m,
            MemInfo {
                total: Some(16_000_000 * 1024),
                available: Some(4_000_000 * 1024),
                swap_total: Some(2000 * 1024),
                swap_free: Some(500 * 1024),
            }
        );
    }

    #[test]
    fn parses_df_even_with_spaces_in_the_filesystem_name() {
        let out = "Filesystem 1024-blocks Used Available Capacity Mounted on\nmap auto_home 0 0 123456 0% /System/Volumes/Data/home\n";
        assert_eq!(parse_df(out), Some(123_456 * 1024));
        let out = "Filesystem     1024-blocks      Used Available Capacity Mounted on\n/dev/disk3s5     971350180 759114504 166734384    82%    /System/Volumes/Data\n";
        assert_eq!(parse_df(out), Some(166_734_384 * 1024));
        assert_eq!(parse_df("garbage"), None);
    }

    #[test]
    fn a_calm_machine_holds_nothing_even_with_sticky_swap() {
        assert_eq!(holds(&calm(), &Gate::default()), Vec::<String>::new());
    }

    #[test]
    fn the_incident_holds_on_every_signal_it_showed() {
        // 2026-09-26: load 1,022 on 12 cores, swap 22 of 23.5 GB, memory gone.
        let r = Readings {
            mem_available_bytes: Some(gib(0.5)),
            memory_pressure_level: Some(4),
            swap_total_bytes: Some(gib(23.5)),
            swap_used_bytes: Some(gib(22.0)),
            load1: Some(1022.0),
            ..calm()
        };
        let why = holds(&r, &Gate::default()).join(" / ");
        for want in ["memory 1% available", "memory pressure critical", "swap 94%", "load 1022.0 on 12 cores"] {
            assert!(why.contains(want), "missing {want:?} in {why:?}");
        }
    }

    #[test]
    fn swap_counts_only_when_memory_is_tight() {
        let g = Gate::default();
        let full_swap = Readings {
            swap_used_bytes: Some(gib(18.9)),
            ..calm()
        };
        assert!(holds(&full_swap, &g).is_empty(), "sticky swap alone must not hold");
        let warn = Readings {
            memory_pressure_level: Some(2),
            ..full_swap.clone()
        };
        let why = holds(&warn, &g);
        assert!(why.iter().any(|w| w.starts_with("swap ")), "{why:?}");
        let low_avail = Readings {
            mem_available_bytes: Some(gib(5.0)), // 7.8% < 2 × 5%
            ..full_swap
        };
        assert!(holds(&low_avail, &g).iter().any(|w| w.starts_with("swap ")));
    }

    #[test]
    fn load_holds_above_its_per_core_line_only() {
        let g = Gate::default();
        assert!(holds(&Readings { load1: Some(48.0), ..calm() }, &g).is_empty(), "exactly 4.0/core is not over");
        assert_eq!(holds(&Readings { load1: Some(49.0), ..calm() }, &g), vec!["load 49.0 on 12 cores"]);
    }

    #[test]
    fn low_disk_names_the_volume() {
        let r = Readings {
            disks: vec![
                Disk {
                    path: "/ok".into(),
                    free_bytes: Some(gib(100.0)),
                },
                Disk {
                    path: "/target".into(),
                    free_bytes: Some(gib(3.0)),
                },
            ],
            ..calm()
        };
        assert_eq!(holds(&r, &Gate::default()), vec!["disk 3.0 GB free at /target"]);
    }

    #[test]
    fn unknown_readings_never_hold() {
        let r = Readings {
            cores: 12,
            ..Readings::default()
        };
        assert!(holds(&r, &Gate::default()).is_empty());
    }

    #[test]
    fn a_zero_threshold_turns_its_signal_off() {
        let r = Readings {
            load1: Some(1000.0),
            memory_pressure_level: Some(4),
            ..calm()
        };
        let g = Gate {
            max_load_per_core: 0.0,
            max_memory_pressure_level: 0,
            ..Gate::default()
        };
        // Load and the pressure signal are off. Swap is still on, and it
        // judges "memory is tight" by the kernel's level whether or not that
        // level is itself a hold signal — so calm()'s 93% swap now counts.
        assert_eq!(holds(&r, &g), vec!["swap 93%"]);
        let g = Gate { max_swap_used_pct: 0.0, ..g };
        assert!(holds(&r, &g).is_empty(), "{:?}", holds(&r, &g));
    }

    /// The real probe on this machine: every field it can read on this OS is
    /// read, and parses to something sane. Not a gate test — just proof the
    /// parsers match what the OS actually prints.
    #[test]
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn the_system_probe_reads_this_machine() {
        let tmp = tempfile::tempdir().unwrap();
        let r = SystemProbe.read(&[tmp.path().to_path_buf()]);
        assert!(r.cores >= 1);
        assert!(r.mem_total_bytes.unwrap_or(0) > 0, "{r:?}");
        assert!(r.mem_available_bytes.is_some(), "{r:?}");
        assert!(r.load1.is_some(), "{r:?}");
        assert!(r.swap_used_pct().is_some(), "{r:?}");
        assert!(r.disks[0].free_bytes.is_some(), "{r:?}");
    }
}

//! `th ci-queue shim` — PATH shims that put heavy tools through the queue for
//! every caller on the machine, including agents that have never heard of it.
//!
//! A shim is a small POSIX-sh file named after the tool (`cargo`, `xcodebuild`,
//! `gradle`) in a directory that comes BEFORE the real binary on PATH
//! (`~/.local/bin` by default). It runs
//! `th ci-queue run --class heavy [--lock cargo] -- <real tool> "$@"`, except:
//!
//! - **Inside a queued job** (`SMOOTH_CI_QUEUE_SLOT` set) it runs the tool
//!   directly. That is the recursion guard: a cargo build script calling cargo,
//!   or turbo calling tsgo, must not queue behind its own parent's slot or lock
//!   — with one heavy slot that is a deadlock.
//! - **Opted out**: `CI_QUEUE=off` (also `0`, `false`, `no`).
//! - **No `th` that can run the queue**: the tool runs directly. The shim
//!   probes the `th`s recorded at install time, then the one on PATH, with
//!   `th ci-queue run --help`, and queues through the first that answers. If
//!   one exists but none can run `ci-queue` (an older `th` took over the
//!   PATH), it says so in one stderr line and runs the tool unqueued — it
//!   never fails the build.
//! - **Light commands** never queue: `cargo --version`, `cargo metadata`,
//!   `cargo fmt`, `gradle --stop`, any `--help`, … — the allowlist below, which
//!   is part of the contract and is tested.
//!
//! The real binary is found by walking PATH and skipping every file that
//! carries [`MARKER_FAMILY`], any version — so a shim never resolves to itself or to another shim,
//! wherever it sits on PATH. The path found at install time is baked in as a
//! fallback.
//!
//! Install is idempotent, and it upgrades a shim written by an older version
//! in place. It never overwrites a file it did not write, unless
//! `--force`, which moves the file aside; uninstall removes exactly what was
//! recorded in `<queue dir>/shims.json` and moves anything it set aside back.
//! No shell rc file is touched: the shim dir must already be on PATH ahead of
//! the real tools, and `status` checks that.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

/// First-lines marker written into every shim this version generates.
///
/// v2 (th-35d0d0): v1 ran `exec th ci-queue run …` whenever ANY `th` was on
/// PATH. On 2026-09-28 the Big Smooth desktop app, relaunching after a reboot,
/// repointed `~/.local/bin/th` at its bundled th 0.54.0, which has no
/// `ci-queue` — and for two hours every heavy cargo, xcodebuild and gradle run
/// on the machine died with "unrecognized subcommand 'ci-queue'". v2 probes
/// for a `th` that can run the queue and falls back to the real tool.
pub const MARKER: &str = "th-ci-queue-shim v2";

/// What every shim version's marker starts with. Detection matches on this,
/// so a v1 shim is still recognised as ours: `install` upgrades it in place
/// and the PATH walk never resolves to it.
pub const MARKER_FAMILY: &str = "th-ci-queue-shim v";

/// The probe a shim runs before queueing: exits 0 only on a `th` whose
/// `ci-queue run` exists. ~10 ms, and only on heavy invocations.
pub const PROBE: &str = "ci-queue run --help";

/// What `install` shims when no `--tools` is given. turbo and tsgo are not
/// here on purpose: pnpm runs them from `node_modules/.bin`, which it puts
/// ahead of everything on PATH, so a shim in `~/.local/bin` never sees them.
///
/// `cargo-nextest` is here for the direct-binary spelling: `cargo nextest …`
/// already queues through the cargo shim, but `cargo-nextest nextest run`
/// then runs rustup's cargo by ABSOLUTE path, so without its own shim the
/// whole build ran outside the queue (seen live 2026-09-28, th-35d0d0).
///
/// `swift` and `xcrun` are here because the iOS work that escaped the queue
/// never touched `xcodebuild`: at load 300–900 on 2026-09-30, 32
/// `swift-frontend`s came from agents' `swift test` / `swift build` on
/// SwiftPM packages, and a few from `xcrun xcodebuild`, which runs Xcode's
/// binary by absolute path past the xcodebuild shim (th-cb3c66).
///
/// A repo's `./gradlew` is a per-repo script no PATH shim ever sees, so the
/// `gradle` shim does NOT cover Android builds that use the wrapper; the repo
/// queues its own wrapper (smooai: SMOODEV-3530).
pub const DEFAULT_TOOLS: &[&str] = &["cargo", "cargo-nextest", "xcodebuild", "gradle", "swift", "xcrun"];

/// Tools whose shim takes the machine's `cargo` lock.
const CARGO_LOCK_TOOLS: &[&str] = &["cargo", "cargo-nextest"];

/// A word a tool's own argv starts with before the real subcommand, skipped
/// once when finding it: cargo runs `cargo-nextest nextest <sub> …`.
fn lead_word(tool: &str) -> Option<&'static str> {
    (tool == "cargo-nextest").then_some("nextest")
}

/// Tools that only RUN another tool (`xcrun xcodebuild …`): the other tools
/// that can be heavy, and the tool's own options that take a value. Anything
/// else they run (`xcrun simctl`, `xcrun --show-sdk-path`) is light, and a
/// heavy-capable one is light or heavy by its own [`light_commands`].
fn dispatch(tool: &str) -> Option<(&'static [&'static str], &'static [&'static str])> {
    (tool == "xcrun").then_some((&["xcodebuild", "swift"][..], &["--sdk", "-sdk", "--toolchain", "-toolchain"][..]))
}

/// Tools a shim can be generated for, with the light subcommands and flags
/// that must never queue. A [`dispatch`] tool has none of its own.
pub fn light_commands(tool: &str) -> Option<&'static [&'static str]> {
    Some(match tool {
        // Metadata, formatting, registry and scaffolding commands: fast, and
        // nothing that builds.
        "cargo" => &[
            "",
            "-V",
            "--version",
            "version",
            "help",
            "--list",
            "metadata",
            "fmt",
            "locate-project",
            "pkgid",
            "tree",
            "search",
            "clean",
            "read-manifest",
            "verify-project",
            "config",
            "login",
            "logout",
            "owner",
            "yank",
            "info",
            "init",
            "new",
            "add",
            "remove",
            "rm",
            "update",
            "generate-lockfile",
        ],
        "xcodebuild" => &[
            "",
            "-version",
            "-list",
            "-showsdks",
            "-showBuildSettings",
            "-showdestinations",
            "-showTestPlans",
            "-checkFirstLaunchStatus",
            "-usage",
            "-license",
            "-find",
            "-find-executable",
            "-find-library",
        ],
        // `list` is NOT light: nextest compiles every test binary to list it.
        "cargo-nextest" => &["", "help", "show-config", "self"],
        "gradle" | "gradlew" => &["", "-v", "--version", "--status", "--stop", "help", "tasks"],
        // `build`, `test`, `run` and a bare script compile; the REPL and
        // package metadata do not.
        "swift" => &["", "repl", "package", "format", "sdk"],
        "xcrun" => &[],
        "turbo" => &["", "--version", "ls", "login", "logout", "link", "unlink", "daemon", "info", "telemetry"],
        "tsgo" | "tsc" => &["", "-v", "--version", "--init", "--showConfig"],
        _ => return None,
    })
}

/// Anywhere in the args, these make any invocation light.
const HELP_FLAGS: &[&str] = &["--help", "-h", "-help", "--version", "-V", "-version"];

/// Dash-prefixed words that ARE the subcommand (xcodebuild's `-list`,
/// gradle's `--stop`, …). Any other leading `-x` is an option and skipped.
const DASH_SUBCOMMANDS: &[&str] = &[
    "-version",
    "-list",
    "-showsdks",
    "-showBuildSettings",
    "-showdestinations",
    "-showTestPlans",
    "-checkFirstLaunchStatus",
    "-usage",
    "-license",
    "-find",
    "-find-executable",
    "-find-library",
    "--status",
    "--stop",
    "-v",
    "--list",
    "--init",
    "--showConfig",
];

/// Whether `tool args…` is a light command that must never queue. The shim
/// script makes the same decision in sh, generated from the same tables; the
/// tests pin the two together.
pub fn is_light(tool: &str, args: &[String]) -> bool {
    let Some(light) = light_commands(tool) else {
        return false;
    };
    if args.iter().any(|a| HELP_FLAGS.contains(&a.as_str())) {
        return true;
    }
    if let Some((inner, value_opts)) = dispatch(tool) {
        let mut it = args.iter().enumerate();
        while let Some((i, a)) = it.next() {
            if value_opts.contains(&a.as_str()) {
                it.next();
            } else if !a.starts_with('-') {
                return !inner.contains(&a.as_str()) || is_light(a, &args[i + 1..]);
            }
        }
        return true;
    }
    let mut sub = "";
    let mut skip = lead_word(tool);
    for a in args {
        if a.starts_with('+') {
            continue;
        }
        if a.starts_with('-') {
            if DASH_SUBCOMMANDS.contains(&a.as_str()) {
                sub = a;
                break;
            }
            continue;
        }
        if skip == Some(a.as_str()) {
            skip = None;
            continue;
        }
        sub = a;
        break;
    }
    light.contains(&sub)
}

/// Single-quote `s` for sh.
fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// The shim script for `tool`, with `real` as the install-time fallback and
/// `ths` as the `th`s to try (in order) before the one on PATH.
pub fn script(tool: &str, real: &Path, ths: &[PathBuf]) -> Result<String> {
    let Some(light) = light_commands(tool) else {
        bail!("no shim is defined for `{tool}` (supported: cargo, cargo-nextest, xcodebuild, gradle, swift, xcrun, gradlew, turbo, tsgo, tsc)");
    };
    let lock = if CARGO_LOCK_TOOLS.contains(&tool) { " --lock cargo" } else { "" };
    let alts = |words: &[&str]| words.iter().map(|c| format!("'{c}'")).collect::<Vec<_>>().join(" | ");
    let help_case = alts(HELP_FLAGS);
    let dash_case = alts(DASH_SUBCOMMANDS);
    // `sub_of LEAD ARGS…` sets $sub: the first argument that is not an option
    // or a +toolchain (or LEAD, once).
    let sub_of = format!(
        r#"sub_of() {{
    skip=$1
    shift
    sub=''
    for a in "$@"; do
        case "$a" in
            +*) continue ;;
            -*)
                case "$a" in {dash_case}) sub=$a; break ;; esac
                continue
                ;;
            *)
                if [ -n "$skip" ] && [ "$a" = "$skip" ]; then skip=''; continue; fi
                sub=$a; break
                ;;
        esac
    done
}}"#
    );
    let decide = match dispatch(tool) {
        None => format!(
            r#"# Light commands never queue. The subcommand is the first argument that is not
# an option or a +toolchain (or the tool's own lead word, once).
{sub_of}
sub_of '{lead}' "$@"
case "$sub" in
    {light_case}) exec "$real" "$@" ;;
esac"#,
            lead = lead_word(tool).unwrap_or(""),
            light_case = alts(light),
        ),
        Some((inner, value_opts)) => {
            let inner_cases: String = inner
                .iter()
                .map(|t| {
                    format!(
                        "\n        {t}) case \"$sub\" in {}) exec \"$real\" \"$@\" ;; esac ;;",
                        alts(light_commands(t).unwrap_or(&[]))
                    )
                })
                .collect();
            format!(
                r#"# {tool} only runs another tool. Only {inner_list} can be heavy, and
# then by that tool's own light list; anything else {tool} runs never queues.
{sub_of}
inner=''
n=0
value=''
for a in "$@"; do
    n=$((n + 1))
    if [ -n "$value" ]; then value=''; continue; fi
    case "$a" in
        {value_case}) value=1 ;;
        -*) ;;
        *) inner=$a; break ;;
    esac
done
case "$inner" in
    {inner_case}) ;;
    *) exec "$real" "$@" ;;
esac
inner_args() {{
    shift "$n"
    sub_of '' "$@"
}}
inner_args "$@"
case "$inner" in{inner_cases}
esac"#,
                inner_list = inner.iter().map(|t| format!("`{t}`")).collect::<Vec<_>>().join(" and "),
                value_case = alts(value_opts),
                inner_case = alts(inner),
            )
        }
    };
    let real = sh_quote(&real.display().to_string());
    let ths: String = ths.iter().map(|p| format!("{} ", sh_quote(&p.display().to_string()))).collect();
    Ok(format!(
        r#"#!/bin/sh
# {MARKER} — {tool}
# Installed by `th ci-queue shim install`; remove with `th ci-queue shim uninstall`.
# Runs heavy {tool} invocations through the machine-wide queue (SMOODEV-3355).
tool='{tool}'
fallback={real}

# The real {tool}: the first one on PATH that is not a shim. Never ourselves.
real=''
old_ifs=$IFS
IFS=:
for d in $PATH; do
    [ -n "$d" ] || continue
    c="$d/$tool"
    [ -f "$c" ] && [ -x "$c" ] || continue
    if head -n 3 "$c" 2>/dev/null | grep -q '{MARKER_FAMILY}'; then continue; fi
    real=$c
    break
done
IFS=$old_ifs
[ -n "$real" ] || real=$fallback

# Inside a queued job: the recursion guard. Never queue behind our own parent.
[ -n "${{SMOOTH_CI_QUEUE_SLOT:-}}" ] && exec "$real" "$@"
case "${{CI_QUEUE:-}}" in off | 0 | false | no) exec "$real" "$@" ;; esac

# --help (and friends) anywhere never queues.
for a in "$@"; do
    case "$a" in
        {help_case}) exec "$real" "$@" ;;
    esac
done

{decide}

# The th that runs the queue: the first that can actually run `ci-queue` —
# the ones recorded at install, then PATH's. A th that cannot (an older one
# took over the PATH link) must never fail the build: warn, run unqueued.
th=''
tried=''
for c in {ths}"$(command -v th 2>/dev/null)"; do
    [ -n "$c" ] && [ -f "$c" ] && [ -x "$c" ] || continue
    case " $tried " in *" $c "*) continue ;; esac
    if "$c" {PROBE} >/dev/null 2>&1; then th=$c; break; fi
    tried="$tried $c"
done
if [ -z "$th" ]; then
    [ -z "$tried" ] || echo "th-ci-queue-shim: no th here can run ci-queue (tried:$tried); running $tool unqueued. Upgrade th, then run: th ci-queue shim install" >&2
    exec "$real" "$@"
fi

label=$(printf '%s' "$tool $*" | cut -c1-60)
exec "$th" ci-queue run --class heavy --label "$label"{lock} -- "$real" "$@"
"#
    ))
}

/// Whether `path` is one of our shims, any version.
pub fn is_shim(path: &Path) -> bool {
    shim_version(path).is_some()
}

/// The version of the shim at `path` (`1`, `2`, …), `None` when it is not one
/// of ours. A marker with an unparseable version still counts as ours (`0`).
pub fn shim_version(path: &Path) -> Option<u32> {
    let b = fs::read(path).ok()?;
    let head: Vec<u8> = b.into_iter().take(512).collect();
    let text = String::from_utf8_lossy(&head);
    text.lines().take(3).find_map(|l| {
        let rest = &l[l.find(MARKER_FAMILY)? + MARKER_FAMILY.len()..];
        let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
        Some(digits.parse().unwrap_or(0))
    })
}

/// The version [`MARKER`] writes.
pub fn current_version() -> u32 {
    MARKER[MARKER_FAMILY.len()..].parse().unwrap_or(0)
}

/// Where Homebrew links `th` (Apple Silicon, Intel, Linux).
pub const BREW_TH_LINKS: &[&str] = &["/opt/homebrew/bin/th", "/usr/local/bin/th", "/home/linuxbrew/.linuxbrew/bin/th"];

/// The default [`BREW_TH_LINKS`] as paths.
pub fn brew_th_links() -> Vec<PathBuf> {
    BREW_TH_LINKS.iter().map(PathBuf::from).collect()
}

/// Homebrew's `th` (`smooai/tools/th`), when installed: the first of `links`
/// that resolves into a `Cellar/th/<version>/bin/th` keg. A `/usr/local/bin/th`
/// that is someone else's link (Big Smooth's, a dev build's) is not brew's.
pub fn brew_th(links: &[PathBuf]) -> Option<PathBuf> {
    links
        .iter()
        .find(|l| fs::canonicalize(l).is_ok_and(|c| brew_keg_formula(&c) == Some("th")))
        .cloned()
}

/// The `th`s a shim should try before PATH's. Homebrew's `th` owns `th` when
/// it is installed, so it comes first; then the `th` running `install`
/// (which, being this code, can run the queue).
///
/// Paths are canonicalised: `~/.local/bin/th` is exactly the kind of link
/// someone else can repoint. A Homebrew keg (`<prefix>/Cellar/<f>/<ver>/bin/th`)
/// also yields `<prefix>/bin/th`, first — brew keeps that link on the newest
/// version, while the keg itself disappears at `brew cleanup`. Every
/// candidate is probed at run time, so a stale one costs nothing but a
/// failed `test -x`.
pub fn queue_th_candidates(exe: &Path, brew_links: &[PathBuf]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(brew) = brew_th(brew_links) {
        out.push(brew);
    }
    let canon = fs::canonicalize(exe).unwrap_or_else(|_| exe.to_path_buf());
    if let Some(stable) = homebrew_stable_link(&canon) {
        out.push(stable);
    }
    out.push(canon);
    let mut seen = std::collections::HashSet::new();
    out.retain(|p| seen.insert(p.clone()));
    out
}

/// When Homebrew's `th` exists but `th` on `path_var` is something else:
/// `(what callers get, brew's)`. `None` when brew has no `th`, nothing is on
/// PATH, or PATH's `th` resolves to brew's.
pub fn th_shadowing_brew(path_var: &str, brew_links: &[PathBuf]) -> Option<(PathBuf, PathBuf)> {
    let brew = brew_th(brew_links)?;
    let first = first_on_path("th", path_var)?;
    let same = fs::canonicalize(&first).ok().is_some_and(|f| fs::canonicalize(&brew).ok() == Some(f));
    (!same).then_some((first, brew))
}

/// One line naming what shadows Homebrew's `th`, for `status` and `th doctor`.
pub fn shadowing_warning(first: &Path, brew: &Path) -> String {
    let resolved = fs::canonicalize(first).map_or_else(|_| String::new(), |c| format!(" (→ {})", c.display()));
    format!(
        "`th` on PATH is {}{resolved}, which shadows Homebrew's {}. Remove it (Big Smooth ≥ 0.59.2 removes its own link) or put {} first on PATH.",
        first.display(),
        brew.display(),
        brew.parent().map_or_else(|| brew.display().to_string(), |d| d.display().to_string())
    )
}

/// `<prefix>/Cellar/<formula>/<version>/bin/<name>` → `<formula>`.
fn brew_keg_formula(p: &Path) -> Option<&str> {
    let bin = p.parent()?;
    if bin.file_name()? != "bin" {
        return None;
    }
    let formula = bin.parent()?.parent()?;
    if formula.parent()?.file_name()? != "Cellar" {
        return None;
    }
    formula.file_name()?.to_str()
}

/// `<prefix>/Cellar/<formula>/<version>/bin/<name>` → `<prefix>/bin/<name>`.
fn homebrew_stable_link(p: &Path) -> Option<PathBuf> {
    brew_keg_formula(p)?;
    let cellar = p.parent()?.parent()?.parent()?.parent()?;
    Some(cellar.parent()?.join("bin").join(p.file_name()?))
}

/// The first executable `tool` on `path_var` that is not a shim.
pub fn find_real(tool: &str, path_var: &str) -> Option<PathBuf> {
    std::env::split_paths(path_var).map(|d| d.join(tool)).find(|c| is_executable(c) && !is_shim(c))
}

/// The first executable `tool` on `path_var`, shim or not — what a caller gets.
pub fn first_on_path(tool: &str, path_var: &str) -> Option<PathBuf> {
    std::env::split_paths(path_var).map(|d| d.join(tool)).find(|c| is_executable(c))
}

fn is_executable(p: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        p.is_file()
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    pub tool: String,
    pub path: PathBuf,
    /// The real binary at install time (the shim's fallback).
    pub real: PathBuf,
    /// A non-shim file `--force` moved aside, restored on uninstall.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub moved_aside: Option<PathBuf>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    pub shims: Vec<Record>,
}

impl State {
    pub fn load(file: &Path) -> Result<Self> {
        match fs::read_to_string(file) {
            Ok(t) => serde_json::from_str(&t).with_context(|| format!("parsing {}", file.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e).with_context(|| format!("reading {}", file.display())),
        }
    }

    pub fn save(&self, file: &Path) -> Result<()> {
        if self.shims.is_empty() {
            match fs::remove_file(file) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
                _ => return Ok(()),
            }
        }
        if let Some(d) = file.parent() {
            fs::create_dir_all(d)?;
        }
        fs::write(file, serde_json::to_vec_pretty(self)?).with_context(|| format!("writing {}", file.display()))
    }
}

/// What `install` did for one tool.
#[derive(Debug, PartialEq, Eq)]
pub enum Installed {
    Wrote {
        path: PathBuf,
        real: PathBuf,
    },
    /// Replaced a shim written by an older version (`from`).
    Upgraded {
        path: PathBuf,
        real: PathBuf,
        from: u32,
    },
    Unchanged {
        path: PathBuf,
    },
    NotFound,
}

/// Write (or refresh, or upgrade) shims for `tools` into `dir`. `path_var` is
/// the PATH to resolve the real tools on; `ths` are the queue-capable `th`s
/// the shim tries before PATH's (see [`queue_th_candidates`]).
pub fn install(dir: &Path, tools: &[String], path_var: &str, ths: &[PathBuf], force: bool, state_file: &Path) -> Result<Vec<(String, Installed)>> {
    fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let mut state = State::load(state_file)?;
    let mut out = Vec::new();
    for tool in tools {
        let Some(real) = find_real(tool, path_var) else {
            out.push((tool.clone(), Installed::NotFound));
            continue;
        };
        let body = script(tool, &real, ths)?;
        let path = dir.join(tool);
        let old_version = shim_version(&path);
        let moved_aside = if path.exists() && !is_shim(&path) {
            if !force {
                bail!(
                    "{} exists and is not a th shim — refusing to overwrite it.\n  Move it yourself, or pass --force to set it aside as {} (restored by uninstall).",
                    path.display(),
                    aside(&path).display()
                );
            }
            let to = aside(&path);
            fs::rename(&path, &to).with_context(|| format!("moving {} aside", path.display()))?;
            Some(to)
        } else {
            None
        };
        if fs::read_to_string(&path).is_ok_and(|t| t == body) {
            out.push((tool.clone(), Installed::Unchanged { path }));
            continue;
        }
        let tmp = path.with_extension("th-shim-tmp");
        fs::write(&tmp, &body).with_context(|| format!("writing {}", tmp.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&tmp, fs::Permissions::from_mode(0o755))?;
        }
        fs::rename(&tmp, &path).with_context(|| format!("installing {}", path.display()))?;
        let previous_aside = state.shims.iter().find(|r| r.path == path).and_then(|r| r.moved_aside.clone());
        state.shims.retain(|r| r.path != path);
        state.shims.push(Record {
            tool: tool.clone(),
            path: path.clone(),
            real: real.clone(),
            moved_aside: moved_aside.or(previous_aside),
        });
        out.push(match old_version {
            Some(from) if from < current_version() => (tool.clone(), Installed::Upgraded { path, real, from }),
            _ => (tool.clone(), Installed::Wrote { path, real }),
        });
    }
    state.save(state_file)?;
    Ok(out)
}

fn aside(path: &Path) -> PathBuf {
    path.with_extension("ci-queue-orig")
}

/// Remove every recorded shim, and move back anything `--force` set aside.
/// A recorded path that no longer holds our shim is left alone.
pub fn uninstall(state_file: &Path) -> Result<Vec<String>> {
    let state = State::load(state_file)?;
    let mut notes = Vec::new();
    for r in &state.shims {
        if is_shim(&r.path) {
            fs::remove_file(&r.path).with_context(|| format!("removing {}", r.path.display()))?;
            notes.push(format!("removed {}", r.path.display()));
        } else if r.path.exists() {
            notes.push(format!("left {} alone: it is no longer a th shim", r.path.display()));
        }
        if let Some(orig) = &r.moved_aside {
            if orig.exists() && !r.path.exists() {
                fs::rename(orig, &r.path).with_context(|| format!("restoring {}", r.path.display()))?;
                notes.push(format!("restored {}", r.path.display()));
            }
        }
    }
    State::default().save(state_file)?;
    Ok(notes)
}

/// One row of `status`.
#[derive(Debug, Serialize)]
pub struct Row {
    pub tool: String,
    pub path: PathBuf,
    pub present: bool,
    /// The shim's version, when it is one of ours. Older than
    /// [`current_version`] means `install` should be re-run.
    pub version: Option<u32>,
    /// The real binary the shim resolves to right now.
    pub real_now: Option<PathBuf>,
    /// What a caller running `<tool>` gets first on PATH.
    pub first_on_path: Option<PathBuf>,
    /// The shim is what callers get.
    pub active: bool,
}

pub fn status(state_file: &Path, path_var: &str) -> Result<Vec<Row>> {
    Ok(State::load(state_file)?
        .shims
        .into_iter()
        .map(|r| {
            let first = first_on_path(&r.tool, path_var);
            let version = shim_version(&r.path);
            Row {
                present: version.is_some(),
                version,
                real_now: find_real(&r.tool, path_var),
                active: first.as_deref() == Some(r.path.as_path()),
                first_on_path: first,
                tool: r.tool,
                path: r.path,
            }
        })
        .collect())
}

#[cfg(all(test, unix))]
#[allow(clippy::unwrap_used, clippy::expect_used, reason = "unwrap is the idiom for test assertions")]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;

    fn exe(path: &Path, body: &str) {
        fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// Which fake `th` a fixture puts on PATH.
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Th {
        None,
        /// Answers the probe, logs `th <args>`, then runs the command after
        /// `--` in a slot (so a queued call still reaches the real tool).
        Capable,
        /// Like th 0.54.0: no `ci-queue` subcommand at all.
        Old,
    }

    /// A `th` that can run the queue, written to `path`, logging to `log`.
    fn capable_th(path: &Path, log: &Path) {
        exe(
            path,
            &format!(
                "for a in \"$@\"; do [ \"$a\" = --help ] && exit 0; done\necho \"th $*\" >> '{}'\nwhile [ \"$#\" -gt 0 ] && [ \"$1\" != -- ]; do shift; done\nshift\nSMOOTH_CI_QUEUE_SLOT=heavy-1 exec \"$@\"",
                log.display()
            ),
        );
    }

    /// shims dir (first on PATH), a "real" bin dir with fake tools that log how
    /// they were called (and exit 7 when an argument is `fail`), and a fake
    /// `th` per [`Th`].
    struct Fx {
        tmp: tempfile::TempDir,
        shims: PathBuf,
        real: PathBuf,
        log: PathBuf,
        state: PathBuf,
    }

    fn fx(with_th: bool) -> Fx {
        fx_with(if with_th { Th::Capable } else { Th::None })
    }

    fn fx_with(th: Th) -> Fx {
        let tmp = tempfile::tempdir().unwrap();
        let shims = tmp.path().join("shims");
        let real = tmp.path().join("real");
        let log = tmp.path().join("log");
        fs::create_dir_all(&shims).unwrap();
        fs::create_dir_all(&real).unwrap();
        for tool in ["cargo", "cargo-nextest", "xcodebuild", "gradle", "swift", "xcrun"] {
            exe(
                &real.join(tool),
                &format!(
                    "echo \"real {tool} $* slot=${{SMOOTH_CI_QUEUE_SLOT:-}}\" >> '{}'\nfor a in \"$@\"; do [ \"$a\" = fail ] && exit 7; done\nexit 0",
                    log.display()
                ),
            );
        }
        match th {
            Th::None => {}
            Th::Capable => capable_th(&real.join("th"), &log),
            Th::Old => exe(
                &real.join("th"),
                &format!(
                    "echo \"old-th $*\" >> '{}'\ncase \"$1\" in ci-queue) echo \"error: unrecognized subcommand 'ci-queue'\" >&2; exit 2 ;; esac\nexit 0",
                    log.display()
                ),
            ),
        }
        let state = tmp.path().join("shims.json");
        Fx { tmp, shims, real, log, state }
    }

    impl Fx {
        fn path_var(&self) -> String {
            format!("{}:{}:/usr/bin:/bin", self.shims.display(), self.real.display())
        }

        fn install(&self, tools: &[&str]) -> Vec<(String, Installed)> {
            self.install_with(tools, &[])
        }

        fn install_with(&self, tools: &[&str], ths: &[PathBuf]) -> Vec<(String, Installed)> {
            let tools: Vec<String> = tools.iter().map(ToString::to_string).collect();
            install(&self.shims, &tools, &self.path_var(), ths, false, &self.state).unwrap()
        }

        /// Run `<tool> args…` as a caller would (PATH lookup), return the log.
        fn call(&self, tool: &str, args: &[&str], env: &[(&str, &str)]) -> String {
            let (code, log, _) = self.run(tool, args, env);
            assert_eq!(code, Some(0), "{tool} {args:?} failed: {log}");
            log
        }

        /// Run `<tool> args…`; (exit code, log, stderr).
        fn run(&self, tool: &str, args: &[&str], env: &[(&str, &str)]) -> (Option<i32>, String, String) {
            let _ = fs::remove_file(&self.log);
            let mut c = Command::new(tool);
            c.args(args)
                .env("PATH", self.path_var())
                .env_remove("SMOOTH_CI_QUEUE_SLOT")
                .env_remove("CI_QUEUE");
            for (k, v) in env {
                c.env(k, v);
            }
            let out = c.output().unwrap();
            (
                out.status.code(),
                fs::read_to_string(&self.log).unwrap_or_default(),
                String::from_utf8_lossy(&out.stderr).into_owned(),
            )
        }
    }

    #[test]
    fn install_writes_executable_shims_and_skips_missing_tools() {
        let f = fx(true);
        let out = f.install(&["cargo", "gradle", "nosuchtool-not-on-path"]);
        assert!(matches!(out[0].1, Installed::Wrote { .. }));
        assert!(matches!(out[1].1, Installed::Wrote { .. }));
        assert_eq!(out[2].1, Installed::NotFound);
        let p = f.shims.join("cargo");
        assert!(is_shim(&p));
        assert_eq!(fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o755);
        let st = State::load(&f.state).unwrap();
        assert_eq!(st.shims.len(), 2);
        assert_eq!(st.shims[0].real, f.real.join("cargo"));
    }

    #[test]
    fn install_is_idempotent() {
        let f = fx(true);
        f.install(&["cargo"]);
        let before = fs::read_to_string(&f.state).unwrap();
        let out = f.install(&["cargo"]);
        assert!(matches!(out[0].1, Installed::Unchanged { .. }), "{out:?}");
        assert_eq!(fs::read_to_string(&f.state).unwrap(), before);
    }

    #[test]
    fn refuses_to_overwrite_a_file_it_did_not_write() {
        let f = fx(true);
        exe(&f.shims.join("cargo"), "echo mine");
        let tools = vec!["cargo".to_string()];
        let err = install(&f.shims, &tools, &f.path_var(), &[], false, &f.state).unwrap_err();
        assert!(err.to_string().contains("is not a th shim"), "{err}");
        assert_eq!(fs::read_to_string(f.shims.join("cargo")).unwrap(), "#!/bin/sh\necho mine\n");
    }

    #[test]
    fn uninstall_restores_exactly_the_prior_state() {
        let f = fx(true);
        exe(&f.shims.join("cargo"), "echo mine");
        fs::write(f.shims.join("unrelated"), "keep me").unwrap();
        let before: Vec<(String, Vec<u8>, u32)> = snapshot(&f.shims);
        let tools = vec!["cargo".to_string(), "gradle".to_string()];
        install(&f.shims, &tools, &f.path_var(), &[], true, &f.state).unwrap();
        assert!(is_shim(&f.shims.join("cargo")), "--force set the file aside and shimmed");
        uninstall(&f.state).unwrap();
        assert_eq!(snapshot(&f.shims), before, "uninstall did not restore the directory");
        assert!(!f.state.exists(), "an empty record is removed");
    }

    fn snapshot(dir: &Path) -> Vec<(String, Vec<u8>, u32)> {
        let mut v: Vec<_> = fs::read_dir(dir)
            .unwrap()
            .map(|e| {
                let e = e.unwrap();
                let m = e.metadata().unwrap().permissions().mode() & 0o777;
                (e.file_name().to_string_lossy().into_owned(), fs::read(e.path()).unwrap(), m)
            })
            .collect();
        v.sort();
        v
    }

    #[test]
    fn uninstall_leaves_a_replaced_file_alone() {
        let f = fx(true);
        f.install(&["cargo"]);
        exe(&f.shims.join("cargo"), "echo someone-else");
        uninstall(&f.state).unwrap();
        assert_eq!(fs::read_to_string(f.shims.join("cargo")).unwrap(), "#!/bin/sh\necho someone-else\n");
    }

    #[test]
    fn a_heavy_call_goes_through_the_queue_to_the_real_tool_never_the_shim() {
        let f = fx(true);
        f.install(&["cargo", "xcodebuild", "gradle"]);
        let log = f.call("cargo", &["build", "-p", "x"], &[]);
        let real = f.real.join("cargo");
        assert!(
            log.contains(&format!(
                "th ci-queue run --class heavy --label cargo build -p x --lock cargo -- {} build -p x",
                real.display()
            )),
            "{log}"
        );
        assert!(log.contains("real cargo build -p x slot=heavy-1"), "{log}");
        let (code, log, _) = f.run("cargo", &["build", "fail"], &[]);
        assert_eq!(code, Some(7), "the tool's exit status comes back through the queue: {log}");
        assert!(log.contains("th ci-queue run"), "{log}");
        let log = f.call("gradle", &["assembleDebug"], &[]);
        assert!(log.contains("th ci-queue run --class heavy --label gradle assembleDebug -- "), "{log}");
        assert!(!log.contains("--lock"), "only cargo and cargo-nextest take the cargo lock: {log}");
        f.install(&["cargo-nextest"]);
        let log = f.call("cargo-nextest", &["nextest", "run", "-p", "smooai-voice"], &[]);
        assert!(
            log.contains("th ci-queue run --class heavy --label cargo-nextest nextest run -p smooai-voice --lock cargo -- "),
            "{log}"
        );
        assert!(log.contains("real cargo-nextest nextest run -p smooai-voice slot=heavy-1"), "{log}");
    }

    /// Table-driven: the light allowlist is part of the contract.
    #[test]
    fn light_commands_never_queue_and_heavy_ones_do() {
        let f = fx(true);
        f.install(DEFAULT_TOOLS);
        let cases: &[(&str, &[&str], bool)] = &[
            ("cargo", &["--version"], false),
            ("cargo", &["-V"], false),
            ("cargo", &[], false),
            ("cargo", &["metadata", "--format-version", "1"], false),
            ("cargo", &["fmt", "--check"], false),
            ("cargo", &["+nightly", "fmt"], false),
            ("cargo", &["build", "--help"], false),
            ("cargo", &["tree", "-i", "x"], false),
            ("cargo", &["clean"], false),
            ("cargo", &["build"], true),
            ("cargo", &["+nightly", "test"], true),
            ("cargo", &["-q", "clippy", "--all-targets"], true),
            ("cargo", &["nextest", "run"], true),
            ("xcodebuild", &["-version"], false),
            ("xcodebuild", &["-list"], false),
            ("xcodebuild", &["-showBuildSettings", "-scheme", "App"], false),
            ("xcodebuild", &["-scheme", "App", "build"], true),
            ("gradle", &["--version"], false),
            ("gradle", &["--stop"], false),
            ("gradle", &["assembleRelease"], true),
            // The direct-binary spelling cargo itself uses: `cargo-nextest nextest <sub>`.
            ("cargo-nextest", &["nextest", "run", "-p", "smooai-voice"], true),
            ("cargo-nextest", &["nextest", "--workspace", "run"], true),
            // `list` builds every test binary to list it.
            ("cargo-nextest", &["nextest", "list"], true),
            ("cargo-nextest", &["nextest", "archive", "--archive-file", "a.tar.zst"], true),
            ("cargo-nextest", &[], false),
            ("cargo-nextest", &["nextest"], false),
            ("cargo-nextest", &["--version"], false),
            ("cargo-nextest", &["nextest", "--version"], false),
            ("cargo-nextest", &["nextest", "run", "--help"], false),
            ("cargo-nextest", &["nextest", "show-config", "test-groups"], false),
            ("cargo-nextest", &["nextest", "self", "update"], false),
            ("cargo-nextest", &["nextest", "help"], false),
            // SwiftPM: what agents ran outside the queue (th-cb3c66).
            ("swift", &["test"], true),
            ("swift", &["build", "-c", "release"], true),
            ("swift", &["run", "tool"], true),
            ("swift", &["test", "--filter", "X"], true),
            ("swift", &[], false),
            ("swift", &["--version"], false),
            ("swift", &["package", "resolve"], false),
            ("swift", &["package", "describe", "--type", "json"], false),
            ("swift", &["build", "--help"], false),
            // xcrun queues only the heavy tools it runs, by their own rules.
            ("xcrun", &["xcodebuild", "-scheme", "App", "test"], true),
            ("xcrun", &["--sdk", "iphonesimulator", "xcodebuild", "build"], true),
            ("xcrun", &["swift", "test"], true),
            ("xcrun", &["--toolchain", "swift", "swift", "build"], true),
            ("xcrun", &["xcodebuild", "-list"], false),
            ("xcrun", &["xcodebuild", "-version"], false),
            ("xcrun", &["swift", "package", "resolve"], false),
            ("xcrun", &["swift", "--version"], false),
            ("xcrun", &["simctl", "boot", "X"], false),
            ("xcrun", &["xcresulttool", "get", "--path", "a.xcresult"], false),
            ("xcrun", &["--show-sdk-path"], false),
            ("xcrun", &["--sdk", "macosx", "--show-sdk-path"], false),
            ("xcrun", &["-f", "xcodebuild"], false),
            ("xcrun", &[], false),
        ];
        for (tool, args, queued) in cases {
            let log = f.call(tool, args, &[]);
            assert_eq!(log.contains("th ci-queue run"), *queued, "{tool} {args:?}: {log}");
            let owned: Vec<String> = args.iter().map(ToString::to_string).collect();
            assert_eq!(is_light(tool, &owned), !*queued, "is_light disagrees with the sh shim on {tool} {args:?}");
            assert!(log.contains(&format!("real {tool}")), "{tool} {args:?} never reached the real tool: {log}");
        }
    }

    /// Mutation-checked: removing the SMOOTH_CI_QUEUE_SLOT line from the
    /// template makes this queue (and the integration test deadlock).
    #[test]
    fn inside_a_queued_job_the_shim_runs_the_tool_directly() {
        let f = fx(true);
        f.install(&["cargo"]);
        let log = f.call("cargo", &["build"], &[("SMOOTH_CI_QUEUE_SLOT", "heavy-1")]);
        assert!(!log.contains("th ci-queue"), "{log}");
        assert!(log.contains("real cargo build"), "{log}");
    }

    #[test]
    fn ci_queue_off_opts_out() {
        let f = fx(true);
        f.install(&["cargo"]);
        for v in ["off", "0", "false", "no"] {
            let log = f.call("cargo", &["build"], &[("CI_QUEUE", v)]);
            assert!(!log.contains("th ci-queue") && log.contains("real cargo build"), "CI_QUEUE={v}: {log}");
        }
    }

    #[test]
    fn without_th_the_tool_runs_directly() {
        let f = fx(false);
        f.install(&["cargo"]);
        // PATH has no th at all (not even the machine's): only our dirs + system.
        let (code, log, err) = f.run("cargo", &["build"], &[]);
        assert_eq!(code, Some(0));
        assert!(log.contains("real cargo build slot="), "{log}");
        assert!(!err.contains("th-ci-queue-shim"), "no th at all is not worth a warning: {err}");
    }

    /// The 2026-09-28 incident: an older th (no `ci-queue`) took over PATH.
    /// The build must still run — unqueued, with one warning line — and exit
    /// with the tool's own status. Mutation-checked: dropping the probe from
    /// the template makes this fail with the old th's exit 2.
    #[test]
    fn a_th_that_cannot_run_ci_queue_runs_the_real_tool_unqueued() {
        let f = fx_with(Th::Old);
        f.install(&["cargo", "gradle"]);
        let (code, log, err) = f.run("cargo", &["build", "-p", "x"], &[]);
        assert_eq!(code, Some(0), "{log}\n{err}");
        assert!(log.contains("real cargo build -p x slot=\n"), "ran directly, not in a slot: {log}");
        assert!(!log.contains("old-th ci-queue run --class"), "never queued through the old th: {log}");
        let warning: Vec<&str> = err.lines().filter(|l| l.contains("th-ci-queue-shim")).collect();
        assert_eq!(warning.len(), 1, "exactly one warning line: {err}");
        assert!(
            warning[0].contains("running cargo unqueued") && warning[0].contains(&f.real.join("th").display().to_string()),
            "{err}"
        );
        let (code, log, _) = f.run("cargo", &["build", "fail"], &[]);
        assert_eq!(code, Some(7), "the tool's own exit status: {log}");
        let (code, log, _) = f.run("gradle", &["assembleDebug"], &[]);
        assert_eq!(code, Some(0));
        assert!(log.contains("real gradle assembleDebug"), "{log}");
    }

    /// The durable half: the th recorded at install (brew's) keeps queueing
    /// even after an older th takes over the PATH link.
    #[test]
    fn a_recorded_th_that_can_queue_wins_over_an_old_th_on_path() {
        let f = fx_with(Th::Old);
        let brew = f.tmp.path().join("brew");
        fs::create_dir_all(&brew).unwrap();
        capable_th(&brew.join("th"), &f.log);
        f.install_with(&["cargo"], &[f.tmp.path().join("gone/th"), brew.join("th")]);
        let (code, log, err) = f.run("cargo", &["build"], &[]);
        assert_eq!(code, Some(0), "{log}\n{err}");
        assert!(log.contains("th ci-queue run --class heavy --label cargo build --lock cargo -- "), "{log}");
        assert!(log.contains("real cargo build slot=heavy-1"), "{log}");
        assert!(!log.contains("old-th"), "PATH's th is never even probed once a recorded one answers: {log}");
        assert!(!err.contains("th-ci-queue-shim"), "{err}");
        let (code, _, _) = f.run("cargo", &["test", "fail"], &[]);
        assert_eq!(code, Some(7));
    }

    #[test]
    fn install_upgrades_a_v1_shim_in_place() {
        let f = fx(true);
        let v1 = "#!/bin/sh\n# th-ci-queue-shim v1 — cargo\nexec th ci-queue run --class heavy -- cargo \"$@\"\n";
        exe(&f.shims.join("cargo"), v1.trim_start_matches("#!/bin/sh\n").trim_end());
        assert_eq!(shim_version(&f.shims.join("cargo")), Some(1));
        // No --force: a v1 shim is ours.
        let out = f.install(&["cargo"]);
        assert!(matches!(out[0].1, Installed::Upgraded { from: 1, .. }), "{out:?}");
        assert_eq!(shim_version(&f.shims.join("cargo")), Some(current_version()));
        assert_eq!(current_version(), 2);
        let rows = status(&f.state, &f.path_var()).unwrap();
        assert_eq!(rows[0].version, Some(2));
        // And again: now current.
        assert!(matches!(f.install(&["cargo"])[0].1, Installed::Unchanged { .. }));
    }

    #[test]
    fn a_v2_shim_skips_a_v1_shim_further_down_path() {
        let f = fx(true);
        f.install(&["cargo"]);
        let old = f.tmp.path().join("old-shims");
        fs::create_dir_all(&old).unwrap();
        exe(&old.join("cargo"), "# th-ci-queue-shim v1 — cargo\nexit 99");
        let path = format!("{}:{}:{}:/usr/bin:/bin", f.shims.display(), old.display(), f.real.display());
        assert_eq!(find_real("cargo", &path), Some(f.real.join("cargo")));
        let out = Command::new("cargo")
            .arg("build")
            .env("PATH", &path)
            .env_remove("SMOOTH_CI_QUEUE_SLOT")
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(0));
        assert!(fs::read_to_string(&f.log).unwrap().contains("real cargo build"));
    }

    #[test]
    fn a_homebrew_keg_records_the_stable_link_first() {
        let tmp = tempfile::tempdir().unwrap();
        let keg = tmp.path().join("Cellar/th/0.58.0/bin");
        fs::create_dir_all(&keg).unwrap();
        fs::create_dir_all(tmp.path().join("bin")).unwrap();
        exe(&keg.join("th"), "exit 0");
        std::os::unix::fs::symlink(keg.join("th"), tmp.path().join("bin/th")).unwrap();
        // Invoked through a link someone else could repoint (~/.local/bin/th).
        let local = tmp.path().join("local-th");
        std::os::unix::fs::symlink(tmp.path().join("bin/th"), &local).unwrap();
        let root = fs::canonicalize(tmp.path()).unwrap();
        assert_eq!(
            queue_th_candidates(&local, &[]),
            vec![root.join("bin/th"), root.join("Cellar/th/0.58.0/bin/th")]
        );
        // Not a keg: just the canonical binary.
        let plain = tmp.path().join("plain-th");
        exe(&plain, "exit 0");
        assert_eq!(queue_th_candidates(&plain, &[]), vec![root.join("plain-th")]);
    }

    /// A tmp "brew prefix" with th linked from its Cellar, plus a Big Smooth
    /// style link into an app bundle: (tmp, brew link, bundle link).
    fn brew_and_bundle() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let keg = tmp.path().join("brew/Cellar/th/0.59.2/bin");
        fs::create_dir_all(&keg).unwrap();
        exe(&keg.join("th"), "exit 0");
        fs::create_dir_all(tmp.path().join("brew/bin")).unwrap();
        std::os::unix::fs::symlink("../Cellar/th/0.59.2/bin/th", tmp.path().join("brew/bin/th")).unwrap();
        let res = tmp.path().join("Big Smooth.app/Contents/Resources");
        fs::create_dir_all(&res).unwrap();
        exe(&res.join("th"), "exit 0");
        fs::create_dir_all(tmp.path().join("local")).unwrap();
        std::os::unix::fs::symlink(res.join("th"), tmp.path().join("local/th")).unwrap();
        let (brew, local) = (tmp.path().join("brew/bin/th"), tmp.path().join("local/th"));
        (tmp, brew, local)
    }

    #[test]
    fn brew_th_is_only_a_link_into_a_th_keg() {
        let (tmp, brew, bundle) = brew_and_bundle();
        assert_eq!(brew_th(&[bundle.clone(), brew.clone()]), Some(brew.clone()), "the bundle link is not brew's");
        assert_eq!(brew_th(&[bundle]), None);
        assert_eq!(brew_th(&[tmp.path().join("nope/th")]), None);
        // Another formula's keg is not th's.
        let other = tmp.path().join("brew/Cellar/other/1.0/bin");
        fs::create_dir_all(&other).unwrap();
        exe(&other.join("th"), "exit 0");
        assert_eq!(brew_th(&[other.join("th")]), None);
    }

    /// Brew owns `th`: its link is tried first, whoever ran `install`.
    #[test]
    fn brew_th_comes_first_even_when_another_th_installs() {
        let (_tmp, brew, bundle) = brew_and_bundle();
        let got = queue_th_candidates(&bundle, std::slice::from_ref(&brew));
        assert_eq!(got[0], brew, "{got:?}");
        assert_eq!(got.len(), 2, "brew's link, then the installer's canonical path: {got:?}");
        // Installed BY brew's th: no duplicate of brew's link.
        let got = queue_th_candidates(&brew, std::slice::from_ref(&brew));
        assert_eq!(got[0], brew);
        assert_eq!(got.iter().filter(|p| **p == brew).count(), 1, "{got:?}");
    }

    #[test]
    fn a_th_ahead_of_brews_on_path_is_named() {
        let (tmp, brew, bundle) = brew_and_bundle();
        let local = bundle.parent().unwrap().display().to_string();
        let brew_dir = brew.parent().unwrap().display().to_string();
        let (first, b) = th_shadowing_brew(&format!("{local}:{brew_dir}"), std::slice::from_ref(&brew)).expect("shadowed");
        assert_eq!((first.as_path(), b.as_path()), (bundle.as_path(), brew.as_path()));
        let msg = shadowing_warning(&first, &b);
        assert!(msg.contains("Big Smooth.app") && msg.contains(&brew.display().to_string()), "{msg}");
        // Brew first: fine. A link that resolves to brew's keg: also fine.
        assert_eq!(th_shadowing_brew(&format!("{brew_dir}:{local}"), std::slice::from_ref(&brew)), None);
        let alias = tmp.path().join("alias");
        fs::create_dir_all(&alias).unwrap();
        std::os::unix::fs::symlink(&brew, alias.join("th")).unwrap();
        assert_eq!(th_shadowing_brew(&format!("{}:{local}", alias.display()), std::slice::from_ref(&brew)), None);
        // No brew th: nothing to shadow.
        assert_eq!(th_shadowing_brew(&local, &[tmp.path().join("nope/th")]), None);
    }

    #[test]
    fn shim_version_reads_the_marker() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("x");
        fs::write(&p, "#!/bin/sh\n# th-ci-queue-shim v12 — cargo\n").unwrap();
        assert_eq!(shim_version(&p), Some(12));
        fs::write(&p, "#!/bin/sh\necho mine\n").unwrap();
        assert_eq!(shim_version(&p), None);
        assert!(script("cargo", Path::new("/bin/cargo"), &[]).unwrap().contains(MARKER));
    }

    #[test]
    fn two_shim_dirs_never_resolve_to_each_other() {
        let f = fx(true);
        f.install(&["cargo"]);
        let second = f.tmp.path().join("shims2");
        fs::create_dir_all(&second).unwrap();
        fs::copy(f.shims.join("cargo"), second.join("cargo")).unwrap();
        let path = format!("{}:{}:{}:/usr/bin:/bin", f.shims.display(), second.display(), f.real.display());
        assert_eq!(find_real("cargo", &path), Some(f.real.join("cargo")));
        let st = Command::new("cargo")
            .arg("--version")
            .env("PATH", &path)
            .env_remove("SMOOTH_CI_QUEUE_SLOT")
            .status()
            .unwrap();
        assert!(st.success());
        assert!(fs::read_to_string(&f.log).unwrap().contains("real cargo --version"));
    }

    #[test]
    fn status_reports_active_shims_and_their_real_tools() {
        let f = fx(true);
        f.install(&["cargo"]);
        let rows = status(&f.state, &f.path_var()).unwrap();
        assert_eq!(rows.len(), 1);
        assert!(rows[0].present && rows[0].active);
        assert_eq!(rows[0].real_now, Some(f.real.join("cargo")));
        // Shim dir AFTER the real tool on PATH: installed, but not active.
        let late = format!("{}:{}", f.real.display(), f.shims.display());
        let rows = status(&f.state, &late).unwrap();
        assert!(rows[0].present && !rows[0].active);
    }

    #[test]
    fn unknown_tools_have_no_shim() {
        assert!(script("rm", Path::new("/bin/rm"), &[]).is_err());
        for t in DEFAULT_TOOLS {
            assert!(light_commands(t).is_some(), "{t}");
        }
    }
}

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
//! - **No `th` on PATH**: the tool runs directly, exactly as before.
//! - **Light commands** never queue: `cargo --version`, `cargo metadata`,
//!   `cargo fmt`, `gradle --stop`, any `--help`, … — the allowlist below, which
//!   is part of the contract and is tested.
//!
//! The real binary is found by walking PATH and skipping every file that
//! carries [`MARKER`] — so a shim never resolves to itself or to another shim,
//! wherever it sits on PATH. The path found at install time is baked in as a
//! fallback.
//!
//! Install is idempotent. It never overwrites a file it did not write, unless
//! `--force`, which moves the file aside; uninstall removes exactly what was
//! recorded in `<queue dir>/shims.json` and moves anything it set aside back.
//! No shell rc file is touched: the shim dir must already be on PATH ahead of
//! the real tools, and `status` checks that.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

/// First-lines marker that identifies a file as one of our shims.
pub const MARKER: &str = "th-ci-queue-shim v1";

/// What `install` shims when no `--tools` is given. turbo and tsgo are not
/// here on purpose: pnpm runs them from `node_modules/.bin`, which it puts
/// ahead of everything on PATH, so a shim in `~/.local/bin` never sees them.
pub const DEFAULT_TOOLS: &[&str] = &["cargo", "xcodebuild", "gradle"];

/// Tools a shim can be generated for, with the light subcommands and flags
/// that must never queue.
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
        "gradle" | "gradlew" => &["", "-v", "--version", "--status", "--stop", "help", "tasks"],
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
    let mut sub = "";
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
        sub = a;
        break;
    }
    light.contains(&sub)
}

/// The shim script for `tool`, with `real` as the install-time fallback.
pub fn script(tool: &str, real: &Path) -> Result<String> {
    let Some(light) = light_commands(tool) else {
        bail!("no shim is defined for `{tool}` (supported: cargo, xcodebuild, gradle, gradlew, turbo, tsgo, tsc)");
    };
    let lock = if tool == "cargo" { " --lock cargo" } else { "" };
    let alts = |words: &[&str]| words.iter().map(|c| format!("'{c}'")).collect::<Vec<_>>().join(" | ");
    let light_case = alts(light);
    let help_case = alts(HELP_FLAGS);
    let dash_case = alts(DASH_SUBCOMMANDS);
    let real = real.display().to_string().replace('\'', "'\\''");
    Ok(format!(
        r#"#!/bin/sh
# {MARKER} — {tool}
# Installed by `th ci-queue shim install`; remove with `th ci-queue shim uninstall`.
# Runs heavy {tool} invocations through the machine-wide queue (SMOODEV-3355).
tool='{tool}'
fallback='{real}'

# The real {tool}: the first one on PATH that is not a shim. Never ourselves.
real=''
old_ifs=$IFS
IFS=:
for d in $PATH; do
    [ -n "$d" ] || continue
    c="$d/$tool"
    [ -f "$c" ] && [ -x "$c" ] || continue
    if head -n 3 "$c" 2>/dev/null | grep -q '{MARKER}'; then continue; fi
    real=$c
    break
done
IFS=$old_ifs
[ -n "$real" ] || real=$fallback

# Inside a queued job: the recursion guard. Never queue behind our own parent.
[ -n "${{SMOOTH_CI_QUEUE_SLOT:-}}" ] && exec "$real" "$@"
case "${{CI_QUEUE:-}}" in off | 0 | false | no) exec "$real" "$@" ;; esac
command -v th >/dev/null 2>&1 || exec "$real" "$@"

# Light commands never queue. The subcommand is the first argument that is not
# an option or a +toolchain.
sub=''
for a in "$@"; do
    case "$a" in
        {help_case}) exec "$real" "$@" ;;
    esac
done
for a in "$@"; do
    case "$a" in
        +*) continue ;;
        -*)
            case "$a" in {dash_case}) sub=$a; break ;; esac
            continue
            ;;
        *) sub=$a; break ;;
    esac
done
case "$sub" in
    {light_case}) exec "$real" "$@" ;;
esac

label=$(printf '%s' "$tool $*" | cut -c1-60)
exec th ci-queue run --class heavy --label "$label"{lock} -- "$real" "$@"
"#
    ))
}

/// Whether `path` is one of our shims.
pub fn is_shim(path: &Path) -> bool {
    fs::read(path).is_ok_and(|b| {
        let head: Vec<u8> = b.into_iter().take(512).collect();
        String::from_utf8_lossy(&head).lines().take(3).any(|l| l.contains(MARKER))
    })
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
    Wrote { path: PathBuf, real: PathBuf },
    Unchanged { path: PathBuf },
    NotFound,
}

/// Write (or refresh) shims for `tools` into `dir`. `path_var` is the PATH to
/// resolve the real tools on.
pub fn install(dir: &Path, tools: &[String], path_var: &str, force: bool, state_file: &Path) -> Result<Vec<(String, Installed)>> {
    fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let mut state = State::load(state_file)?;
    let mut out = Vec::new();
    for tool in tools {
        let Some(real) = find_real(tool, path_var) else {
            out.push((tool.clone(), Installed::NotFound));
            continue;
        };
        let body = script(tool, &real)?;
        let path = dir.join(tool);
        let mut moved_aside = None;
        if path.exists() && !is_shim(&path) {
            if !force {
                bail!(
                    "{} exists and is not a th shim — refusing to overwrite it.\n  Move it yourself, or pass --force to set it aside as {} (restored by uninstall).",
                    path.display(),
                    aside(&path).display()
                );
            }
            let to = aside(&path);
            fs::rename(&path, &to).with_context(|| format!("moving {} aside", path.display()))?;
            moved_aside = Some(to);
        }
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
        out.push((tool.clone(), Installed::Wrote { path, real }));
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
            Row {
                present: is_shim(&r.path),
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

    /// shims dir (first on PATH), a "real" bin dir with fake tools that log how
    /// they were called, and a fake `th` that logs its args and then runs the
    /// command after `--` (so a queued call still reaches the real tool).
    struct Fx {
        tmp: tempfile::TempDir,
        shims: PathBuf,
        real: PathBuf,
        log: PathBuf,
        state: PathBuf,
    }

    fn fx(with_th: bool) -> Fx {
        let tmp = tempfile::tempdir().unwrap();
        let shims = tmp.path().join("shims");
        let real = tmp.path().join("real");
        let log = tmp.path().join("log");
        fs::create_dir_all(&shims).unwrap();
        fs::create_dir_all(&real).unwrap();
        for tool in ["cargo", "xcodebuild", "gradle"] {
            exe(
                &real.join(tool),
                &format!("echo \"real {tool} $* slot=${{SMOOTH_CI_QUEUE_SLOT:-}}\" >> '{}'", log.display()),
            );
        }
        if with_th {
            exe(
                &real.join("th"),
                &format!(
                    "echo \"th $*\" >> '{}'\nwhile [ \"$#\" -gt 0 ] && [ \"$1\" != -- ]; do shift; done\nshift\nSMOOTH_CI_QUEUE_SLOT=heavy-1 exec \"$@\"",
                    log.display()
                ),
            );
        }
        let state = tmp.path().join("shims.json");
        Fx { tmp, shims, real, log, state }
    }

    impl Fx {
        fn path_var(&self) -> String {
            format!("{}:{}:/usr/bin:/bin", self.shims.display(), self.real.display())
        }

        fn install(&self, tools: &[&str]) -> Vec<(String, Installed)> {
            let tools: Vec<String> = tools.iter().map(ToString::to_string).collect();
            install(&self.shims, &tools, &self.path_var(), false, &self.state).unwrap()
        }

        /// Run `<tool> args…` as a caller would (PATH lookup), return the log.
        fn call(&self, tool: &str, args: &[&str], env: &[(&str, &str)]) -> String {
            let _ = fs::remove_file(&self.log);
            let mut c = Command::new(tool);
            c.args(args)
                .env("PATH", self.path_var())
                .env_remove("SMOOTH_CI_QUEUE_SLOT")
                .env_remove("CI_QUEUE");
            for (k, v) in env {
                c.env(k, v);
            }
            let st = c.status().unwrap();
            assert!(st.success(), "{tool} {args:?} failed: {st:?}");
            fs::read_to_string(&self.log).unwrap_or_default()
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
        let err = install(&f.shims, &tools, &f.path_var(), false, &f.state).unwrap_err();
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
        install(&f.shims, &tools, &f.path_var(), true, &f.state).unwrap();
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
        let log = f.call("gradle", &["assembleDebug"], &[]);
        assert!(log.contains("th ci-queue run --class heavy --label gradle assembleDebug -- "), "{log}");
        assert!(!log.contains("--lock"), "only cargo takes the cargo lock: {log}");
    }

    /// Table-driven: the light allowlist is part of the contract.
    #[test]
    fn light_commands_never_queue_and_heavy_ones_do() {
        let f = fx(true);
        f.install(&["cargo", "xcodebuild", "gradle"]);
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
        let log = f.call("cargo", &["build"], &[]);
        assert!(log.contains("real cargo build slot="), "{log}");
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
        assert!(script("rm", Path::new("/bin/rm")).is_err());
        for t in DEFAULT_TOOLS {
            assert!(light_commands(t).is_some(), "{t}");
        }
    }
}

//! Link libghostty-vt for the daemon's headless terminal (th-5025fb).
//!
//! The static library comes from the repo's `scripts/ghostty-vt/build-ghostty-vt.sh`,
//! pinned by the `ghostty-vt.lock` beside it — the same script and pin SmoothFlow
//! Desktop builds from, so the daemon snapshots with the exact Ghostty every
//! client parses with. This runs the script when the library for the target
//! being compiled is missing; the script is a no-op once its stamp matches.
//!
//! Where it builds, in order:
//! - `GHOSTTY_VT_DIR=<dir>`: a prebuilt `<dir>/{include,lib}`; the script never runs.
//! - `GHOSTTY_VT_WORK=<dir>`: the script's work dir (CI points it inside the
//!   checkout so `actions/cache` can keep `<dir>/out`).
//! - otherwise a per-user cache, `<cache>/smooth/ghostty-vt/<ghostty commit>`,
//!   so every worktree on a machine shares one build instead of fetching Zig and
//!   compiling Ghostty per checkout. The script serializes concurrent builds.
//!
//! `csrc/flow_vt.c`, the small C bridge `src/ffi.rs` calls, is compiled
//! against the library's headers.

// A build script reports failure by panicking; there is no caller to return to.
#![allow(clippy::expect_used, clippy::panic, clippy::option_if_let_else)]

use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let target = env::var("TARGET").expect("TARGET");
    let windows = target.contains("windows");
    let lib_file = if windows { "ghostty-vt-static.lib" } else { "libghostty-vt.a" };
    let scripts = manifest.join("..").join("..").join("scripts").join("ghostty-vt");
    let lock = scripts.join("ghostty-vt.lock");

    println!("cargo:rerun-if-env-changed=GHOSTTY_VT_DIR");
    println!("cargo:rerun-if-env-changed=GHOSTTY_VT_WORK");
    println!("cargo:rerun-if-changed=csrc/flow_vt.c");
    println!("cargo:rerun-if-changed={}", lock.display());
    println!("cargo:rerun-if-changed={}", scripts.join("build-ghostty-vt.sh").display());

    let dir = if let Some(dir) = env::var_os("GHOSTTY_VT_DIR") {
        PathBuf::from(dir)
    } else {
        let work = env::var_os("GHOSTTY_VT_WORK").map_or_else(|| default_work(&lock), PathBuf::from);
        build_with_script(&scripts, &work, &target);
        work.join("out").join(&target)
    };
    let lib_dir = dir.join("lib");
    let include = dir.join("include");
    assert!(
        lib_dir.join(lib_file).is_file() && include.join("ghostty").join("vt.h").is_file(),
        "libghostty-vt for {target} is missing from {} — run scripts/ghostty-vt/build-ghostty-vt.sh {target}",
        dir.display()
    );
    println!("cargo:rerun-if-changed={}", lib_dir.join(lib_file).display());

    cc::Build::new()
        .file("csrc/flow_vt.c")
        .include(&include)
        .define("GHOSTTY_STATIC", None)
        .warnings(true)
        .compile("flow_vt");

    println!("cargo:rustc-link-search=native={}", lib_dir.display());
    println!("cargo:rustc-link-lib=static={}", if windows { "ghostty-vt-static" } else { "ghostty-vt" });
    if windows {
        // Zig's standard library calls NT and kernel32 functions directly.
        println!("cargo:rustc-link-lib=ntdll");
        println!("cargo:rustc-link-lib=kernel32");
    } else if target.contains("apple") {
        // The bundled SIMD code is C++ built without libc++: libSystem suffices.
    } else {
        println!("cargo:rustc-link-lib=m");
    }
}

/// `<user cache>/smooth/ghostty-vt/<first 12 of ghostty_sha>`. Keyed by the
/// pinned commit so worktrees on different pins never rebuild each other's
/// output out from under a link.
fn default_work(lock: &Path) -> PathBuf {
    let text = std::fs::read_to_string(lock).unwrap_or_else(|e| panic!("read {}: {e}", lock.display()));
    let sha = text
        .lines()
        .find_map(|l| l.trim().strip_prefix("ghostty_sha="))
        .map(str::trim)
        .filter(|s| s.len() >= 12)
        .unwrap_or_else(|| panic!("{} has no ghostty_sha", lock.display()));
    let cache = env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            if cfg!(windows) {
                env::var_os("LOCALAPPDATA").map(PathBuf::from)
            } else {
                None
            }
        })
        .or_else(|| env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
        .unwrap_or_else(|| PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR")));
    cache.join("smooth").join("ghostty-vt").join(&sha[..12])
}

/// Run the pinned build script (idempotent). Windows runs it under Git Bash,
/// never WSL's `bash.exe`, which `System32` puts first on PATH.
fn build_with_script(scripts: &Path, work: &Path, target: &str) {
    let script = scripts.join("build-ghostty-vt.sh");
    println!("cargo:rerun-if-env-changed=GHOSTTY_VT_BASH");
    let bash = env::var_os("GHOSTTY_VT_BASH").map_or_else(
        || {
            let git_bash = PathBuf::from(r"C:\Program Files\Git\bin\bash.exe");
            if cfg!(windows) && git_bash.is_file() {
                git_bash
            } else {
                PathBuf::from("bash")
            }
        },
        PathBuf::from,
    );
    // Forward slashes: Git Bash reads `D:/a/…` but not every tool in the
    // script copes with backslashes.
    let slash = |p: &Path| p.to_string_lossy().replace('\\', "/");
    // The script's progress goes to stderr: cargo parses a build script's stdout.
    let status = Command::new(&bash)
        .arg(slash(&script))
        .arg(target)
        .env("GHOSTTY_VT_WORK", slash(work))
        .stdout(std::io::stderr())
        .status();
    match status {
        Ok(s) if s.success() => {}
        Ok(s) => panic!("{} {target} failed ({s})", script.display()),
        Err(e) => panic!(
            "could not run {} with bash ({e}); build libghostty-vt yourself and set GHOSTTY_VT_DIR",
            script.display()
        ),
    }
}

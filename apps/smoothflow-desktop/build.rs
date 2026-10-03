//! Link libghostty-vt, SmoothFlow Desktop's terminal engine (th-872ea8).
//!
//! The static library comes from the repo's shared
//! `scripts/ghostty-vt/build-ghostty-vt.sh` (pinned by the `ghostty-vt.lock`
//! next to it, which `crates/smooth-flow-vt` builds from too), which this runs
//! when the library for the target being compiled is missing; it is a no-op
//! once built. Output lands in `.ghostty-vt/` here (`GHOSTTY_VT_WORK` moves
//! it). `GHOSTTY_VT_DIR` points at a prebuilt `<dir>/{include,lib}` instead
//! and skips the script.
//! `csrc/smoothflow_vt.c`, the small C bridge `src/ghostty.rs` calls, is
//! compiled against its headers.

use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let target = env::var("TARGET").expect("TARGET");
    let windows = target.contains("windows");
    let lib_file = if windows { "ghostty-vt-static.lib" } else { "libghostty-vt.a" };

    println!("cargo:rerun-if-env-changed=GHOSTTY_VT_DIR");
    println!("cargo:rerun-if-changed=csrc/smoothflow_vt.c");
    let scripts = manifest.join("..").join("..").join("scripts").join("ghostty-vt");
    println!("cargo:rerun-if-changed={}", scripts.join("ghostty-vt.lock").display());
    println!("cargo:rerun-if-changed={}", scripts.join("build-ghostty-vt.sh").display());

    let dir = if let Some(dir) = env::var_os("GHOSTTY_VT_DIR") {
        PathBuf::from(dir)
    } else {
        let work = env::var_os("GHOSTTY_VT_WORK").map_or_else(|| manifest.join(".ghostty-vt"), PathBuf::from);
        println!("cargo:rerun-if-env-changed=GHOSTTY_VT_WORK");
        let dir = work.join("out").join(&target);
        build_with_script(&scripts, &work, &target);
        dir
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
        .file("csrc/smoothflow_vt.c")
        .include(&include)
        .define("GHOSTTY_STATIC", None)
        .warnings(true)
        .compile("smoothflow_vt");

    println!("cargo:rustc-link-search=native={}", lib_dir.display());
    println!("cargo:rustc-link-lib=static={}", if windows { "ghostty-vt-static" } else { "ghostty-vt" });
    if windows {
        // Zig's standard library calls NT and kernel32 functions directly
        // (Ghostty's CMakeLists links these for its static consumers too).
        println!("cargo:rustc-link-lib=ntdll");
        println!("cargo:rustc-link-lib=kernel32");
    } else if target.contains("apple") {
        // The SIMD code (simdutf, highway) bundled into the archive is C++
        // built without libc++, so libSystem is all it needs.
    } else {
        println!("cargo:rustc-link-lib=m");
    }
}

/// Run the pinned build script (idempotent: it exits at once when the stamp
/// matches). Windows runs it under Git Bash, never WSL's `bash.exe`, which
/// `System32` puts first on PATH.
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
    let script_arg = script.to_string_lossy().replace('\\', "/");
    // The script's progress goes to stderr: cargo parses a build script's stdout.
    let status = Command::new(&bash)
        .arg(&script_arg)
        .arg(target)
        .env("GHOSTTY_VT_WORK", work.to_string_lossy().replace('\\', "/"))
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

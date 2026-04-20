//! Emit `TH_VERSION` at compile time so Big Smooth's startup log
//! can print "v0.8.0 (abc123)". Duplicates `crates/smooth-cli/build.rs`
//! to keep the value available without a shared helper crate —
//! a few lines of duplication is cheaper than a new workspace member.

use std::process::Command;

fn main() {
    let pkg_version = env!("CARGO_PKG_VERSION");
    let git_sha = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .and_then(|out| {
            if out.status.success() {
                String::from_utf8(out.stdout).ok().map(|s| s.trim().to_string())
            } else {
                None
            }
        })
        .filter(|s| !s.is_empty());

    let version_string = match git_sha {
        Some(sha) => format!("{pkg_version} ({sha})"),
        None => pkg_version.to_string(),
    };

    println!("cargo:rustc-env=TH_VERSION={version_string}");
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    println!("cargo:rerun-if-changed=../../.git/refs");
}

//! Emit `TH_VERSION` at compile time — the crate version plus a
//! short git sha when the binary was built from a git checkout.
//! Lets `th --version` distinguish "old 0.8.0" from "today's 0.8.0"
//! on long-lived services that predate a code change (the workflow
//! flag bug that cost us most of a benchmark day was a classic case).

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

    // Rebuild the version stamp when HEAD moves so `th --version`
    // stays accurate across commits without a full clean build.
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    println!("cargo:rerun-if-changed=../../.git/refs");
}

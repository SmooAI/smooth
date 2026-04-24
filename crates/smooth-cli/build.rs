//! Emit `TH_VERSION` and `BENCH_SCORE_JSON` at compile time.
//!
//! - `TH_VERSION` = crate version + short git sha when built from a
//!   git checkout. Lets `th --version` distinguish "old 0.8.0" from
//!   "today's 0.8.0" on long-lived services that predate a code
//!   change (the workflow flag bug that cost us most of a benchmark
//!   day was a classic case).
//! - `BENCH_SCORE_JSON` = raw contents of `<repo>/docs/bench-latest.json`
//!   at build time. That's The Line — the aider-polyglot pass rate
//!   the release workflow commits for each tag. Empty string when the
//!   file is missing or unreadable so the binary still builds outside
//!   a tagged release and `th bench score` can print a helpful
//!   "not baked in yet" message.

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

    // Bake the current `docs/bench-latest.json` into the binary as
    // BENCH_SCORE_JSON. Missing file or read error → empty string;
    // `th bench score` handles empty by printing a "not baked in
    // yet" message instead of a score. Keeps the build green for
    // dev checkouts that haven't cut a tagged release yet.
    //
    // cargo's `cargo:rustc-env=KEY=VALUE` directive is line-delimited,
    // so newlines inside the JSON would truncate the value. Strip
    // them — the embedded env var is consumed as JSON, which is
    // whitespace-insensitive, so collapsing the pretty-print form to
    // a single line is lossless.
    let score_path = std::path::Path::new("../..").join("docs/bench-latest.json");
    let score_json = std::fs::read_to_string(&score_path).unwrap_or_default().replace(['\n', '\r'], "");
    println!("cargo:rustc-env=BENCH_SCORE_JSON={score_json}");
    println!("cargo:rerun-if-changed={}", score_path.display());
}

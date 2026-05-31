//! Ensure `web/dist/` exists at compile time so `rust-embed` can
//! resolve its `#[folder = "web/dist/"]` annotation even on a fresh
//! worktree where the Vite build hasn't run yet. Without this, the
//! macro expansion fails (`get` not found on `WebAssets`) and the
//! whole workspace fails to build until the user runs
//! `pnpm build:web`.
//!
//! The placeholder gets overwritten on the first real `vite build`
//! and the directory is git-ignored at repo root.

use std::fs;
use std::path::PathBuf;

fn main() {
    // `env!` resolves CARGO_MANIFEST_DIR at compile time (infallible) — no
    // runtime `expect()` for clippy::expect_used to flag.
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let dist = PathBuf::from(manifest_dir).join("web").join("dist");
    let index = dist.join("index.html");

    if index.exists() {
        // Real build artifact present — nothing to do.
        println!("cargo:rerun-if-changed={}", index.display());
        return;
    }

    if let Err(e) = fs::create_dir_all(&dist) {
        // Don't fail the build for a write hiccup; the macro will
        // produce a more useful error than this would.
        eprintln!("warning: smooth-web build.rs could not create {}: {e}", dist.display());
        return;
    }
    let placeholder = b"<!doctype html><meta charset=utf-8><title>smooth web (placeholder)</title>\n<p>This is a build-time placeholder. Run <code>pnpm build:web</code> at the repo root to populate the real SPA.</p>\n";
    if let Err(e) = fs::write(&index, placeholder) {
        eprintln!("warning: smooth-web build.rs could not write {}: {e}", index.display());
    }
    // No `cargo:rerun-if-changed` for the placeholder — vite will
    // overwrite it, and we want subsequent builds to pick up the
    // real bundle without re-triggering this script.
}

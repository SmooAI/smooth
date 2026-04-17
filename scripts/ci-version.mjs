#!/usr/bin/env node
/**
 * CI version script — runs during Changeset version PR creation.
 *
 * 1. `changeset version` — bumps package.json + consumes the changeset files
 * 2. `version:sync` — propagates the new version to Cargo.toml +
 *    workspace.dependencies + Cargo.lock so the version PR carries the
 *    Rust-side bump too. Previously changesets ran step 1 directly (via
 *    its default `version` input), and step 2 only fired during `publish`
 *    — after the PR was already merged with stale Cargo.* files.
 */
import { execSync } from "node:child_process";
import process from "node:process";

const root = process.cwd();

function run(cmd, opts = {}) {
    console.log(`\n> ${cmd}`);
    execSync(cmd, { stdio: "inherit", cwd: root, ...opts });
}

run("pnpm changeset version");
run("pnpm version:sync");

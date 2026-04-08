#!/usr/bin/env node
/**
 * CI publish script for Smooth.
 *
 * Runs changeset version (bumps package.json), syncs version to Cargo.toml/Cargo.lock,
 * and handles the publish flow. Since Smooth is private (no npm/crates.io publish),
 * this just does version bumping + sync.
 */
import { execSync } from "node:child_process";
import process from "node:process";

const root = process.cwd();

function run(cmd, opts = {}) {
    console.log(`\n> ${cmd}`);
    execSync(cmd, { stdio: "inherit", cwd: root, ...opts });
}

// Step 1: Apply changesets (bumps package.json version, updates CHANGELOG.md)
run("pnpm changeset version");

// Step 2: Sync version to Cargo.toml and Cargo.lock
run("pnpm version:sync");

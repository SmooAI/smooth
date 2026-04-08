#!/usr/bin/env node
/**
 * Sync version from package.json → Cargo.toml workspace.package.version + Cargo.lock.
 *
 * Changesets bumps package.json; this script propagates the new version to Rust.
 */
import { readFileSync, writeFileSync } from "node:fs";
import { resolve } from "node:path";
import process from "node:process";

const root = process.cwd();

const packageJsonPath = resolve(root, "package.json");
const pkg = JSON.parse(readFileSync(packageJsonPath, "utf8"));
const version = pkg.version;

if (!version) {
    console.error("Unable to read version from package.json");
    process.exit(1);
}

const updates = [
    {
        path: "Cargo.toml",
        apply(content) {
            // Update workspace.package.version in root Cargo.toml
            const pattern = /(\[workspace\.package\]\s*\nversion\s*=\s*")([^"]+)(")/;
            if (!pattern.test(content)) {
                throw new Error(
                    "workspace.package version line not found in Cargo.toml",
                );
            }
            return content.replace(pattern, `$1${version}$3`);
        },
    },
    {
        path: "Cargo.lock",
        apply(content) {
            // Update the version for all workspace crate entries.
            // Workspace crates all share the same version (workspace.package.version).
            // We match the pattern: name = "smooth-*"\nversion = "X.Y.Z"
            const pattern =
                /(name = "smooth-[^"]+"\nversion = ")([^"]+)(")/g;
            return content.replace(pattern, `$1${version}$3`);
        },
    },
];

let touched = 0;

for (const { path, apply } of updates) {
    const absolutePath = resolve(root, path);
    let content;
    try {
        content = readFileSync(absolutePath, "utf8");
    } catch (error) {
        if (error && error.code === "ENOENT") {
            console.warn(`Skipping ${path} (not found)`);
            continue;
        }
        throw error;
    }
    const next = apply(content);
    if (next !== content) {
        writeFileSync(absolutePath, next);
        touched += 1;
        console.log(`Updated version in ${path}`);
    }
}

if (touched === 0) {
    console.warn("No files were updated by sync-versions.");
} else {
    console.log(`\nSynced version ${version} to ${touched} file(s).`);
}

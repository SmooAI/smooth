#!/usr/bin/env node
/**
 * Sync version from package.json → Cargo.toml + Cargo.lock.
 *
 * Changesets bumps package.json; this script propagates the new version to:
 *
 *   1. `[workspace.package] version`  — root Cargo.toml
 *   2. `[workspace.dependencies]` entries `smooth-X = { version = "x.y.z", ... }`
 *      so publishable crates carry matching version requirements on internal deps
 *   3. Every `smooai-smooth-*` + `smooth-*` entry in Cargo.lock
 *
 * All three must move together or `cargo publish` in CI will either fail
 * validation (version mismatch) or publish a stale lock that subsequent
 * `cargo install` calls refuse.
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
            let next = content;

            // 1. workspace.package.version
            const workspacePattern =
                /(\[workspace\.package\]\s*\nversion\s*=\s*")([^"]+)(")/;
            if (!workspacePattern.test(next)) {
                throw new Error(
                    "workspace.package.version line not found in Cargo.toml",
                );
            }
            next = next.replace(workspacePattern, `$1${version}$3`);

            // 2. workspace.dependencies smooth-X = { ... version = "...", ... }
            //    Only rewrites the `version = "..."` occurrence on the same
            //    line as a smooth-X entry. Lines without a version key are
            //    left alone — we add them in step 3.
            const depVersionPattern =
                /^(smooth-[a-z-]+\s*=\s*\{[^}\n]*\bversion\s*=\s*")([^"]+)(")/gm;
            next = next.replace(depVersionPattern, `$1${version}$3`);

            // 3. Add version to any smooth-X workspace dep that doesn't have
            //    one yet. Match "smooth-X = { path = "crates/smooth-X", ... }"
            //    and splice `version = "X.Y.Z",` in right after the opening brace.
            const addVersionPattern =
                /^(smooth-[a-z-]+\s*=\s*\{)(?!([^}\n]*\bversion\b))([^}\n]*)(\})/gm;
            next = next.replace(
                addVersionPattern,
                (_, pre, _v, body, close) => {
                    const trimmed = body.trimStart();
                    const separator = trimmed.length > 0 ? " " : "";
                    return `${pre} version = "${version}",${separator}${trimmed}${close}`;
                },
            );

            return next;
        },
    },
    {
        path: "Cargo.lock",
        apply(content) {
            // Every workspace crate uses the package name `smooai-smooth-*`
            // (see the `package = "smooai-smooth-<name>"` rename in commit
            // 933b927). The old regex matched `smooth-*` only and silently
            // missed every crate.
            const pattern =
                /(name = "smooai-smooth-[^"]+"\nversion = ")([^"]+)(")/g;
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

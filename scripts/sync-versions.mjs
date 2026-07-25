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

            // An EXTERNAL workspace.dependencies entry whose version must NOT be
            // synced to the workspace version: the crates.io operator-core dep
            // (name-matched — it has no `git =`), or any git dep (its version
            // lives at the pinned rev, not the workspace). See steps 2 & 3.
            const isExternalDep = (s) =>
                s.includes("smooai-smooth-operator-core") || /\bgit\s*=/.test(s);

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
            //    Bump the version requirement on INTERNAL smooth-X deps so the
            //    crates carry matching requirements. Lines without a version
            //    key are left alone — we add them in step 3.
            //
            //    EXCEPTION: EXTERNAL deps published from another repo at their
            //    own cadence — NOT workspace members — must keep their own
            //    version, or `cargo` resolution breaks (workspace X.Y.Z vs the
            //    external crate's real release). Two shapes to skip:
            //      - `smooth-operator` = crates.io dep pinning
            //        `smooai-smooth-operator-core` (no `git =`, name-matched).
            //      - `smooth-operator-server`/`-svc` = GIT deps (pinned by rev);
            //        their crate version at the rev (e.g. 1.23.1) is unrelated
            //        to the workspace version — matched by the `git =` key so
            //        ANY external git dep is covered, not just these two.
            //    Pearl th-1ee32b (git-dep twin the core-only guard missed).
            const depLinePattern = /^(smooth-[a-z-]+\s*=\s*\{[^}\n]*\})$/gm;
            next = next.replace(depLinePattern, (line) => {
                if (isExternalDep(line)) {
                    return line;
                }
                return line.replace(
                    /(\bversion\s*=\s*")([^"]+)(")/,
                    `$1${version}$3`,
                );
            });

            // 3. Add version to any smooth-X workspace dep that doesn't have
            //    one yet. Match "smooth-X = { path = "crates/smooth-X", ... }"
            //    and splice `version = "X.Y.Z",` in right after the opening brace.
            //
            //    SAME EXCEPTION as step 2: skip EXTERNAL deps (crates.io
            //    operator-core AND the operator git deps). This pass targets
            //    exactly the version-less lines step 2 left alone, so without
            //    the guard it injects `version = "<workspace>"` onto the git
            //    deps — pinning an operator release that doesn't exist at the
            //    rev and breaking `cargo` resolution (the 0.23.0-vs-1.23.1
            //    failure). Pearl th-1ee32b.
            const addVersionPattern =
                /^(smooth-[a-z-]+\s*=\s*\{)(?!([^}\n]*\bversion\b))([^}\n]*)(\})/gm;
            next = next.replace(
                addVersionPattern,
                (match, pre, _v, body, close) => {
                    if (isExternalDep(body)) {
                        return match;
                    }
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
            //
            // EXCEPTION: the EXTERNAL operator crates published from the
            // smooth-operator repo — their locked versions track that repo, NOT
            // the workspace version. Bumping a lock entry to a workspace version
            // that source never published breaks `cargo` resolution under
            // `--locked` ("= "*" locked to 0.23.0 … candidate 1.23.1"). Three
            // package names, matched exactly:
            //   - `smooai-smooth-operator-core`   (crates.io engine)
            //   - `smooai-smooth-operator-server` (git dep)
            //   - `smooai-smooth-operator`        (git dep, the svc crate)
            // The Cargo.toml dep skip (above) keys on `git =`, but lock entries
            // carry `source` on a separate line outside this 2-line match, so
            // here we skip by exact name. Pearl th-1ee32b (the lock twin the
            // git-dep Cargo.toml fix in #260 left behind).
            const pattern =
                /(name = "smooai-smooth-[^"]+"\nversion = ")([^"]+)(")/g;
            return content.replace(pattern, (match, pre, _ver, post) => {
                if (/name = "smooai-smooth-operator(-core|-server)?"\n/.test(pre)) {
                    return match;
                }
                return `${pre}${version}${post}`;
            });
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

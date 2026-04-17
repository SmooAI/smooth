#!/usr/bin/env node
/**
 * CI publish script — runs after the Changeset version PR merges.
 *
 * Publishes every workspace crate to crates.io in dependency order.
 *
 * Idempotent: if the target version of a crate is already on crates.io
 * (detected via the sparse index), we skip it. `cargo publish` would
 * otherwise fail the re-run with "a package with this name and version
 * already exists."
 *
 * Requires the env var `CARGO_REGISTRY_TOKEN` (or a prior
 * `cargo login`). The release workflow passes it in.
 */
import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import https from "node:https";
import process from "node:process";

const root = process.cwd();

// Topological order derived from `cargo metadata` (normal deps only).
// Regenerate with:
//   cargo metadata --format-version 1 --no-deps \
//     | jq -r '.packages[] | select(.name | startswith("smooai-smooth-")) | .name'
// …then topo-sort. Update this list when you add a new crate.
//
// Crates marked `publish = false` in their Cargo.toml are intentionally
// omitted. The first-round excludes are:
//   - smooai-smooth-web           (embedded Vite SPA — `web/dist/` is
//                                  gitignored, so the published crate would
//                                  be empty without an `include` fix)
//   - smooai-smooth-bigsmooth     (depends on smooth-web)
//   - smooai-smooth-code          (TUI; hold until smooth-web lands)
//   - smooai-smooth-cli           (binary; distribute via the release
//                                  tarball + `cargo install --git` for now)
//   - smooai-smooth-operator-runner (guest-only binary, not useful on host)
const PUBLISH_ORDER = [
    // Layer 0 — no internal deps
    "smooai-smooth-policy",
    "smooai-smooth-operator",
    "smooai-smooth-bootstrap-bill",
    // Layer 1 — depend on Layer 0
    "smooai-smooth-pearls",
    "smooai-smooth-narc",
    "smooai-smooth-scribe",
    "smooai-smooth-plugin",
    "smooai-smooth-goalie",
    // Layer 2 — depend on Layer 1
    "smooai-smooth-diver",
    "smooai-smooth-archivist",
    "smooai-smooth-wonk",
];

const pkg = JSON.parse(readFileSync(resolve(root, "package.json"), "utf8"));
const version = pkg.version;
if (!version) {
    console.error("Unable to read version from package.json");
    process.exit(1);
}

function run(cmd, args, opts = {}) {
    console.log(`\n> ${cmd} ${args.join(" ")}`);
    execFileSync(cmd, args, { stdio: "inherit", cwd: root, ...opts });
}

// crates.io sparse-index path layout:
//   len 1 → 1/<name>
//   len 2 → 2/<name>
//   len 3 → 3/<first-char>/<name>
//   len ≥ 4 → <first-two>/<next-two>/<name>
function sparsePath(crate) {
    if (crate.length === 1) return `1/${crate}`;
    if (crate.length === 2) return `2/${crate}`;
    if (crate.length === 3) return `3/${crate[0]}/${crate}`;
    return `${crate.slice(0, 2)}/${crate.slice(2, 4)}/${crate}`;
}

function getSparseIndex(crate) {
    const url = `https://index.crates.io/${sparsePath(crate)}`;
    return new Promise((res) => {
        https
            .get(url, (r) => {
                if (r.statusCode === 404) {
                    r.resume();
                    res({ exists: false, versions: [] });
                    return;
                }
                let body = "";
                r.setEncoding("utf8");
                r.on("data", (c) => (body += c));
                r.on("end", () => {
                    const versions = body
                        .split("\n")
                        .map((l) => l.trim())
                        .filter(Boolean)
                        .map((line) => {
                            try {
                                return JSON.parse(line).vers;
                            } catch {
                                return null;
                            }
                        })
                        .filter(Boolean);
                    res({ exists: versions.length > 0, versions });
                });
            })
            .on("error", (err) => {
                console.warn(
                    `sparse-index lookup failed for ${crate}: ${err.message}`,
                );
                // Fall through — cargo publish will reject cleanly if already published.
                res({ exists: true, versions: [] });
            });
    });
}

async function isAlreadyPublished(crate, ver) {
    const { versions } = await getSparseIndex(crate);
    return versions.includes(ver);
}

async function isCrateNew(crate) {
    const { exists } = await getSparseIndex(crate);
    return !exists;
}

function sleep(ms) {
    return new Promise((res) => setTimeout(res, ms));
}

(async () => {
    console.log(`Publishing Smooth workspace @ ${version} to crates.io`);
    console.log(`Order (${PUBLISH_ORDER.length} crates):`);
    for (const name of PUBLISH_ORDER) console.log(`  - ${name}`);

    // crates.io's "new crate" rate limit is strict: you can only publish a
    // few brand-new crate names per minute. A fresh workspace introducing
    // many new crates at once will hit HTTP 429 without throttling. We
    // only pause when the *previous* publish was a new-crate first-ever
    // upload; subsequent version bumps of already-published crates don't
    // hit this limit.
    let lastPublishWasNew = false;
    const newCrateDelayMs = 15_000;

    for (const crate of PUBLISH_ORDER) {
        const published = await isAlreadyPublished(crate, version);
        if (published) {
            console.log(`\n[skip] ${crate}@${version} already on crates.io`);
            continue;
        }

        if (lastPublishWasNew) {
            console.log(
                `  throttling ${newCrateDelayMs / 1000}s to stay under crates.io's new-crate rate limit`,
            );
            await sleep(newCrateDelayMs);
        }

        const willBeNewCrate = await isCrateNew(crate);
        console.log(
            `\n[publish] ${crate}@${version}${willBeNewCrate ? " (first upload)" : ""}`,
        );
        try {
            // --no-verify skips the pre-flight `cargo build --release` that
            // cargo publish runs by default. The release job has already
            // built the workspace before we reach this step, so verifying
            // again triples the run time for no safety gain.
            run("cargo", ["publish", "-p", crate, "--no-verify"]);
            lastPublishWasNew = willBeNewCrate;
        } catch (err) {
            const nowPublished = await isAlreadyPublished(crate, version);
            if (nowPublished) {
                console.log(
                    `  (recovered) ${crate}@${version} appeared on crates.io during publish — continuing`,
                );
                lastPublishWasNew = willBeNewCrate;
                continue;
            }
            throw err;
        }
    }

    console.log(
        `\nPublished ${PUBLISH_ORDER.length} crate(s) @ ${version} to crates.io.`,
    );
})().catch((err) => {
    console.error(err);
    process.exit(1);
});

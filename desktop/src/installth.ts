//! Put the bundled `th` CLI on the user's PATH.
//!
//! The DMG ships `th` next to `smooth-daemon` in the app bundle. We symlink it
//! into a PATH dir so `th` works from a terminal — and because the link points
//! INTO the bundle, an OTA update that replaces the bundle auto-updates the `th`
//! users run. No extra update channel needed.

import { execFileSync } from 'node:child_process';
import { existsSync, lstatSync, mkdirSync, readlinkSync, realpathSync, symlinkSync, unlinkSync } from 'node:fs';
import { homedir } from 'node:os';
import { dirname, join, resolve } from 'node:path';

export type LinkResult = {
    action:
        | 'created'
        | 'repointed'
        | 'current'
        | 'skipped-regular-file'
        | 'skipped-foreign-link'
        | 'skipped-newer'
        | 'skipped-unknown-version'
        | 'no-writable-dir'
        | 'unsupported';
    path?: string;
    note?: string;
};

/** Reads a `th`'s version (`0.58.0`), `undefined` when it can't be run or parsed. */
export type VersionOf = (bin: string) => string | undefined;

/** PATH dirs to try, in order. `/usr/local/bin` usually wins on PATH but needs
 * write access; `~/.local/bin` is the always-writable fallback. */
const DEFAULT_TARGETS = ['/usr/local/bin/th', join(homedir(), '.local', 'bin', 'th')];

/**
 * Symlink `bundledTh` onto PATH.
 *
 * SAFETY RULES (mirrors scripts/dev-link-th.sh): only ever create or repoint a
 * SYMLINK. A regular file at the target is a `th` someone installed on purpose
 * (Homebrew, curl) — leave it, never clobber it. We act on the first target that
 * already exists rather than minting a second `th` that would shadow it.
 *
 * And never DOWNGRADE (th-35d0d0). An existing symlink is repointed only when it
 * is ours — it points into an app bundle (`….app/Contents/Resources/…`) or
 * nowhere at all — and the `th` it runs now is not newer than the bundled one.
 * A link someone else made (`~/.local/bin/th → /opt/homebrew/bin/th`,
 * `scripts/dev-link-th.sh` → a cargo build) is theirs. On 2026-09-28 a relaunch
 * after a reboot repointed exactly that brew link at the bundled th 0.54.0,
 * which predates `th ci-queue`, and every queued cargo/xcodebuild/gradle on the
 * machine failed for two hours.
 */
export function linkThOnPath(bundledTh: string | undefined, targets: string[] = DEFAULT_TARGETS, versionOf: VersionOf = thVersion): LinkResult {
    if (process.platform === 'win32') return { action: 'unsupported', note: 'windows PATH is not managed this way' };
    if (!bundledTh || !existsSync(bundledTh)) return { action: 'unsupported', note: 'no bundled th to link' };

    for (const target of targets) {
        if (!pathPresent(target)) continue;
        if (isSymlink(target)) {
            const dest = readlinkSafe(target);
            if (dest === bundledTh || sameFile(target, bundledTh)) return { action: 'current', path: target };
            const dangling = !existsSync(target);
            if (!dangling && !ownedLinkDest(resolve(dirname(target), dest))) {
                return { action: 'skipped-foreign-link', path: target, note: `points at ${dest}, not an app bundle — not ours to repoint` };
            }
            if (!dangling) {
                const bundled = versionOf(bundledTh);
                const current = versionOf(target);
                if (!bundled) {
                    return { action: 'skipped-unknown-version', path: target, note: `bundled th reported no version; keeping ${dest}` };
                }
                if (current && compareVersions(current, bundled) > 0) {
                    return { action: 'skipped-newer', path: target, note: `${dest} is th ${current}, newer than the bundled ${bundled}` };
                }
            }
            try {
                unlinkSync(target);
                symlinkSync(bundledTh, target);
                return { action: 'repointed', path: target, note: dangling ? `was dangling (${dest})` : `was ${dest}` };
            } catch {
                return { action: 'no-writable-dir', path: target, note: 'could not repoint existing symlink' };
            }
        }
        // Regular file — a deliberately-installed th. Coexist; do not touch it.
        return { action: 'skipped-regular-file', path: target };
    }

    // Nothing on PATH yet — create in the first target whose parent we can write.
    for (const target of targets) {
        try {
            mkdirSync(dirname(target), { recursive: true });
            symlinkSync(bundledTh, target);
            return { action: 'created', path: target };
        } catch {
            // Not writable (e.g. /usr/local/bin without root) — try the next.
        }
    }
    return { action: 'no-writable-dir', note: 'no writable PATH dir among candidates' };
}

/** A link into an app bundle's Resources is one this app (or an older copy of
 * it, the menubar's "Install th CLI…") made. */
export function ownedLinkDest(dest: string): boolean {
    return /\.app\/Contents\/Resources\//.test(dest);
}

/** `th --version` → `0.58.0`. Bounded: a first launch after an OTA can stall
 * while macOS validates the new binary, and this runs on app start. */
export function thVersion(bin: string): string | undefined {
    try {
        return parseVersion(execFileSync(bin, ['--version'], { encoding: 'utf8', timeout: 5000, stdio: ['ignore', 'pipe', 'ignore'] }));
    } catch {
        return undefined;
    }
}

/** The first `major.minor.patch` in `text` (`th 0.58.0 (3719cc9)` → `0.58.0`). */
export function parseVersion(text: string): string | undefined {
    return /(\d+)\.(\d+)\.(\d+)/.exec(text)?.[0];
}

/** Numeric `major.minor.patch` comparison: negative, 0, or positive. */
export function compareVersions(a: string, b: string): number {
    const pa = a.split('.').map(Number);
    const pb = b.split('.').map(Number);
    for (let i = 0; i < 3; i++) {
        const d = (pa[i] ?? 0) - (pb[i] ?? 0);
        if (d !== 0) return d;
    }
    return 0;
}

/** lstat-based existence: true even for a dangling symlink (which existsSync misses). */
function pathPresent(p: string): boolean {
    try {
        lstatSync(p);
        return true;
    } catch {
        return false;
    }
}

function isSymlink(p: string): boolean {
    try {
        return lstatSync(p).isSymbolicLink();
    } catch {
        return false;
    }
}

function readlinkSafe(p: string): string {
    try {
        return readlinkSync(p);
    } catch {
        return '';
    }
}

function sameFile(a: string, b: string): boolean {
    try {
        return realpathSync(a) === realpathSync(b);
    } catch {
        return false;
    }
}

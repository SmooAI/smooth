//! Put the bundled `th` CLI on the user's PATH.
//!
//! The DMG ships `th` next to `smooth-daemon` in the app bundle. We symlink it
//! into a PATH dir so `th` works from a terminal — and because the link points
//! INTO the bundle, an OTA update that replaces the bundle auto-updates the `th`
//! users run. No extra update channel needed.

import { existsSync, lstatSync, mkdirSync, readlinkSync, symlinkSync, unlinkSync } from 'node:fs';
import { homedir } from 'node:os';
import { dirname, join } from 'node:path';

export type LinkResult = {
    action: 'created' | 'repointed' | 'current' | 'skipped-regular-file' | 'no-writable-dir' | 'unsupported';
    path?: string;
    note?: string;
};

/** PATH dirs to try, in order. `/usr/local/bin` usually wins on PATH but needs
 * write access; `~/.local/bin` is the always-writable fallback. */
const DEFAULT_TARGETS = ['/usr/local/bin/th', join(homedir(), '.local', 'bin', 'th')];

/**
 * Symlink `bundledTh` onto PATH.
 *
 * SAFETY RULE (mirrors scripts/dev-link-th.sh): only ever create or repoint a
 * SYMLINK. A regular file at the target is a `th` someone installed on purpose
 * (Homebrew, curl) — leave it, never clobber it. We act on the first target that
 * already exists rather than minting a second `th` that would shadow it.
 */
export function linkThOnPath(bundledTh: string | undefined, targets: string[] = DEFAULT_TARGETS): LinkResult {
    if (process.platform === 'win32') return { action: 'unsupported', note: 'windows PATH is not managed this way' };
    if (!bundledTh || !existsSync(bundledTh)) return { action: 'unsupported', note: 'no bundled th to link' };

    for (const target of targets) {
        if (!pathPresent(target)) continue;
        if (isSymlink(target)) {
            if (readlinkSafe(target) === bundledTh) return { action: 'current', path: target };
            try {
                unlinkSync(target);
                symlinkSync(bundledTh, target);
                return { action: 'repointed', path: target };
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

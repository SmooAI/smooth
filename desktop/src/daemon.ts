//! Resolving, starting, and stopping the native `smooth-daemon` child.
//!
//! The daemon is the engine; this Electron app is only its shell. We attach to
//! an already-running daemon when one answers `/health` on the configured port
//! (the common case on a dev box running `th up` or a launchd unit) and only
//! spawn — and therefore only ever kill — a daemon we started ourselves.

import { type ChildProcess, execFileSync, spawn } from 'node:child_process';
import { existsSync } from 'node:fs';
import { homedir } from 'node:os';
import { delimiter, join } from 'node:path';

const BIN = process.platform === 'win32' ? 'smooth-daemon.exe' : 'smooth-daemon';

/** `host:port` the daemon binds. Mirrors `resolve_run_addr()` in smooth-daemon/src/main.rs. */
export function resolveAddr(env: NodeJS.ProcessEnv = process.env): string {
    const raw = (env.SMOOTH_ADDR ?? '').trim();
    return raw === '' ? '127.0.0.1:8787' : raw;
}

export function baseUrl(env: NodeJS.ProcessEnv = process.env): string {
    return `http://${resolveAddr(env)}`;
}

/**
 * Locate `smooth-daemon`. Same order as `th daemon` (crates/smooth-cli/src/daemon_launcher.rs)
 * with the packaged copy first: bundled resources → `SMOOTH_DAEMON_BIN` → `~/.smooth/bin` →
 * `PATH` → the cargo target dir (dev, honoring a global `build.target-dir`).
 */
export function resolveDaemonBin(candidates: string[] = defaultCandidates()): string | undefined {
    return candidates.find((p) => p !== '' && existsSync(p));
}

function defaultCandidates(): string[] {
    const out: string[] = [];
    if (process.resourcesPath) out.push(join(process.resourcesPath, BIN));
    out.push((process.env.SMOOTH_DAEMON_BIN ?? '').trim());
    out.push(join(homedir(), '.smooth', 'bin', BIN));
    for (const dir of (process.env.PATH ?? '').split(delimiter)) {
        if (dir) out.push(join(dir, BIN));
    }
    for (const target of cargoTargetDirs()) {
        out.push(join(target, 'release', BIN), join(target, 'debug', BIN));
    }
    return out;
}

/** Dev fallback: where `cargo build` puts things. `cargo metadata` is the only thing that
 *  knows about a global `build.target-dir`, so ask it — but only as a last resort, it's slow. */
function cargoTargetDirs(): string[] {
    const out: string[] = [];
    const fromEnv = (process.env.CARGO_TARGET_DIR ?? '').trim();
    if (fromEnv) out.push(fromEnv);
    out.push(join(repoRoot(), 'target'));
    try {
        const meta = execFileSync('cargo', ['metadata', '--no-deps', '--format-version', '1'], {
            cwd: repoRoot(),
            encoding: 'utf8',
            timeout: 30_000,
            stdio: ['ignore', 'pipe', 'ignore'],
        });
        const dir = JSON.parse(meta).target_directory;
        if (typeof dir === 'string') out.push(dir);
    } catch {
        // No cargo, not a workspace, or it timed out — the earlier candidates stand.
    }
    return out;
}

/** The smooth repo root — `desktop/` sits directly under it. */
function repoRoot(): string {
    return join(import.meta.dirname, '..', '..');
}

export async function isHealthy(url = baseUrl()): Promise<boolean> {
    try {
        const res = await fetch(`${url}/health`, { signal: AbortSignal.timeout(1500) });
        return res.ok;
    } catch {
        return false;
    }
}

let child: ChildProcess | undefined;

/**
 * Ensure a daemon is serving at {@link baseUrl}. Returns how that came to be so
 * the caller can report a failure to the user.
 */
export async function startDaemon(): Promise<{ ok: boolean; spawned: boolean; error?: string }> {
    if (await isHealthy()) return { ok: true, spawned: false };

    const bin = resolveDaemonBin();
    if (!bin) {
        return { ok: false, spawned: false, error: `Could not find ${BIN}. Build it with \`pnpm install:th\`, or set SMOOTH_DAEMON_BIN.` };
    }

    child = spawn(bin, ['run'], { stdio: 'inherit', env: { ...process.env, SMOOTH_ADDR: resolveAddr() } });
    child.on('exit', () => {
        child = undefined;
    });

    // ponytail: poll rather than parse the daemon's log for a ready line — /health
    // is the contract, and a 60s ceiling covers a cold sqlite migration on first run.
    const deadline = Date.now() + 60_000;
    while (Date.now() < deadline) {
        if (await isHealthy()) return { ok: true, spawned: true };
        if (!child) return { ok: false, spawned: true, error: `${BIN} exited during startup.` };
        await new Promise((r) => setTimeout(r, 300));
    }
    return { ok: false, spawned: true, error: `${BIN} did not answer ${baseUrl()}/health within 60s.` };
}

/** Terminate the daemon, but only if we were the one who started it. */
export function stopDaemon(): void {
    child?.kill('SIGTERM');
    child = undefined;
}

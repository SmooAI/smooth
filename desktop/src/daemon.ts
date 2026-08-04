//! Resolving, starting, and stopping the native `smooth-daemon` child.
//!
//! The daemon is the engine; this Electron app is only its shell. We attach to
//! an already-running daemon when one answers `/health` on the configured port
//! (the common case on a dev box running `th up` or a launchd unit) and only
//! spawn — and therefore only ever kill — a daemon we started ourselves.

import { type ChildProcess, execFile, execFileSync, spawn } from 'node:child_process';
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
export function resolveDaemonBin(candidates?: string[]): string | undefined {
    if (candidates) return firstExisting(candidates);
    // The cargo lookup shells out, so only reach for it when the cheap candidates
    // miss — in a packaged app the bundled copy is first and always wins.
    return firstExisting(cheapCandidates()) ?? firstExisting(cargoCandidates());
}

function firstExisting(candidates: string[]): string | undefined {
    return candidates.find((p) => p !== '' && existsSync(p));
}

function cheapCandidates(): string[] {
    const out: string[] = [];
    if (process.resourcesPath) out.push(join(process.resourcesPath, BIN));
    out.push((process.env.SMOOTH_DAEMON_BIN ?? '').trim());
    out.push(join(homedir(), '.smooth', 'bin', BIN));
    for (const dir of (process.env.PATH ?? '').split(delimiter)) {
        if (dir) out.push(join(dir, BIN));
    }
    return out;
}

function cargoCandidates(): string[] {
    return cargoTargetDirs().flatMap((dir) => [join(dir, 'release', BIN), join(dir, 'debug', BIN)]);
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

    // SMOOTH_MENUBAR=0: the bundled daemon lives in Contents/MacOS, which is the
    // daemon's own "I was launched as an app, show a status item" signal. We own
    // the tray, so turn its one off.
    child = spawn(bin, ['run'], { stdio: 'inherit', env: { ...process.env, SMOOTH_ADDR: resolveAddr(), SMOOTH_MENUBAR: '0' } });
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

/**
 * Run a one-shot daemon subcommand as OUR child.
 *
 * This is the TCC seam, and it does NOT work yet — see "TCC" in the README. A
 * spawned child inherits the responsible process for grants it already has, but
 * it cannot *ask*: `smooth-daemon tcc calendar` run from here returns
 * not-determined and no prompt appears, while the identical binary launched via
 * `open` as an app bundle's main executable prompts correctly. The plumbing is
 * right; the embedding isn't.
 */
export async function runDaemonCommand(args: string[]): Promise<{ ok: boolean; output: string }> {
    const bin = resolveDaemonBin();
    if (!bin) return { ok: false, output: `Could not find ${BIN}.` };
    return new Promise((resolve) => {
        // No timeout: an EventKit request blocks while the OS prompt waits for
        // an answer, and the daemon already caps that at two minutes.
        execFile(bin, args, (err, stdout, stderr) => {
            const output = `${stdout}${stderr}`.trim();
            resolve({ ok: !err, output: output || (err ? String(err) : '') });
        });
    });
}

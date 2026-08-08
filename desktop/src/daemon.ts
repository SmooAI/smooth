//! Resolving, starting, and stopping the native `smooth-daemon` child.
//!
//! The daemon is the engine; this Electron app is only its shell. We attach to
//! an already-running daemon when one answers `/health` on the configured port
//! (the common case on a dev box running `th up` or a launchd unit) and only
//! spawn — and therefore only ever kill — a daemon we started ourselves.

import { type ChildProcess, execFile, execFileSync, spawn } from 'node:child_process';
import { existsSync, readFileSync } from 'node:fs';
import { homedir } from 'node:os';
import { delimiter, join } from 'node:path';

const BIN = process.platform === 'win32' ? 'smooth-daemon.exe' : 'smooth-daemon';
const TH = process.platform === 'win32' ? 'th.exe' : 'th';

/** Where the running daemon advertises its bound `host:port` (written by
 * smooth-daemon's `persist_daemon_addr`). Lets us find it on whatever port it
 * landed on instead of guessing. */
const DAEMON_ADDR_FILE = join(homedir(), '.smooth', 'daemon.addr');

/**
 * `host:port` the daemon binds. Resolution order:
 *   1. `SMOOTH_ADDR` env (an explicit override — launchd/systemd unit or the user)
 *   2. `~/.smooth/daemon.addr` — what the RUNNING daemon actually bound
 *   3. the `127.0.0.1:8787` default (mirrors `resolve_run_addr()` in the daemon)
 *
 * Step 2 is the fix for hosts where `:8787` is taken (smoo-hub runs the SmooHub
 * dashboard there, so the daemon moves to `:8788`): a double-clicked app gets no
 * launchd env, and without this it would default to `:8787` and load the wrong
 * app. Reading the daemon's advertised addr makes the window follow the daemon
 * wherever it is. (th-8af70d)
 */
export function resolveAddr(env: NodeJS.ProcessEnv = process.env, addrFile: string = DAEMON_ADDR_FILE): string {
    const raw = (env.SMOOTH_ADDR ?? '').trim();
    if (raw !== '') return raw;
    const advertised = readAdvertisedAddr(addrFile);
    return advertised ?? '127.0.0.1:8787';
}

/** Read + validate `~/.smooth/daemon.addr`. Returns undefined on any problem
 * (missing, unreadable, or not a plausible `host:port`) so we cleanly fall back. */
function readAdvertisedAddr(addrFile: string): string | undefined {
    try {
        if (!existsSync(addrFile)) return undefined;
        const v = readFileSync(addrFile, 'utf8').trim();
        return /^[^\s/:]+:\d{1,5}$/.test(v) ? v : undefined;
    } catch {
        return undefined;
    }
}

/** A full remote daemon URL to attach to (a tailnet daemon like smoo-hub) instead
 * of the local one. Empty ⇒ local. main.ts sets it from the saved config before
 * anything reads {@link baseUrl}. */
export function remoteUrl(env: NodeJS.ProcessEnv = process.env): string {
    return (env.SMOOTH_REMOTE_URL ?? '').trim();
}

/** The daemon the window connects to: the remote URL when set, else the local one.
 * A remote daemon serves its own token-injected SPA, so loading this URL is a
 * complete, authenticated connection — no token is passed by the client. */
export function baseUrl(env: NodeJS.ProcessEnv = process.env): string {
    const remote = remoteUrl(env);
    return remote === '' ? `http://${resolveAddr(env)}` : remote.replace(/\/$/, '');
}

/** True when attached to a remote daemon (so: never spawn, never teardown). */
export function isRemote(env: NodeJS.ProcessEnv = process.env): boolean {
    return remoteUrl(env) !== '';
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
    return firstExisting(cheapCandidates(BIN)) ?? firstExisting(cargoCandidates(BIN));
}

/** Locate the bundled `th` CLI. Same candidate order as {@link resolveDaemonBin}
 * (bundled resources first), so a packaged app finds the copy staged next to the
 * daemon; a dev box falls back to `~/.smooth/bin`, PATH, then the cargo target. */
export function resolveThBin(candidates?: string[]): string | undefined {
    if (candidates) return firstExisting(candidates);
    return firstExisting(cheapCandidates(TH)) ?? firstExisting(cargoCandidates(TH));
}

function firstExisting(candidates: string[]): string | undefined {
    return candidates.find((p) => p !== '' && existsSync(p));
}

function cheapCandidates(bin: string): string[] {
    const out: string[] = [];
    if (process.resourcesPath) out.push(join(process.resourcesPath, bin));
    if (bin === BIN) out.push((process.env.SMOOTH_DAEMON_BIN ?? '').trim());
    out.push(join(homedir(), '.smooth', 'bin', bin));
    for (const dir of (process.env.PATH ?? '').split(delimiter)) {
        if (dir) out.push(join(dir, bin));
    }
    return out;
}

function cargoCandidates(bin: string): string[] {
    return cargoTargetDirs().flatMap((dir) => [join(dir, 'release', bin), join(dir, 'debug', bin)]);
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
    // Remote mode: never spawn a local daemon — just confirm the remote one answers.
    if (isRemote()) {
        return (await isHealthy())
            ? { ok: true, spawned: false }
            : { ok: false, spawned: false, error: `The remote daemon at ${baseUrl()} isn’t reachable — is it running and are you on the tailnet?` };
    }
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

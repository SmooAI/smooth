//! Resolving, starting, and stopping the native `smooth-daemon` child.
//!
//! The daemon is the engine; this Electron app is only its shell. We attach to
//! an already-running daemon when one answers `/health` on the configured port
//! (the common case on a dev box running `th up` or a launchd unit) and only
//! spawn — and therefore only ever kill — a daemon we started ourselves.

import { type ChildProcess, execFile, execFileSync, spawn } from 'node:child_process';
import { appendFileSync, existsSync, mkdirSync, openSync, readFileSync } from 'node:fs';
import { homedir } from 'node:os';
import { delimiter, dirname, join } from 'node:path';

const BIN = process.platform === 'win32' ? 'smooth-daemon.exe' : 'smooth-daemon';
const TH = process.platform === 'win32' ? 'th.exe' : 'th';

/** Where the running daemon advertises its bound `host:port` (written by
 * smooth-daemon's `persist_daemon_addr`). Lets us find it on whatever port it
 * landed on instead of guessing. */
const DAEMON_ADDR_FILE = join(homedir(), '.smooth', 'daemon.addr');

/** Where the desktop shell logs — the daemon child's stdout/stderr AND our own
 * spawn diagnostics. Under `open`/Finder there is no terminal, so inherited stdio
 * is lost; a file is the only place the real startup error survives (th-5c2ec6). */
const DESKTOP_LOG_FILE = join(homedir(), '.smooth', 'desktop.log');

export function desktopLogPath(): string {
    return DESKTOP_LOG_FILE;
}

/** Append one timestamped line to the desktop log; never throws (best-effort). */
export function desktopLog(line: string): void {
    try {
        mkdirSync(dirname(DESKTOP_LOG_FILE), { recursive: true });
        appendFileSync(DESKTOP_LOG_FILE, `${new Date().toISOString()} ${line}\n`);
    } catch {
        // Logging must never take the app down.
    }
}

/** stdio for the spawned daemon: redirect its output to the desktop log so it
 * survives a Finder launch, falling back to inherit if the file can't be opened. */
function daemonStdio(): ['ignore', number | 'inherit', number | 'inherit'] {
    try {
        mkdirSync(dirname(DESKTOP_LOG_FILE), { recursive: true });
        const fd = openSync(DESKTOP_LOG_FILE, 'a');
        return ['ignore', fd, fd];
    } catch {
        return ['ignore', 'inherit', 'inherit'];
    }
}

/** The local daemon's base URL — always `http://<resolved addr>`, independent of
 * any remote view target. The app owns its own daemon at this address. */
function localUrl(): string {
    return `http://${resolveAddr()}`;
}

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

/**
 * Path to the nested TCC helper bundle inside a packaged Big Smooth.app, or
 * `undefined` off macOS / when unpackaged (no `resourcesPath`). The helper's
 * main executable is `smooth-daemon`, so launching it via `open` lets macOS
 * attribute the EventKit prompt to Big Smooth — a spawned daemon child cannot
 * ask (see after-pack.mjs). `resourcesPath` is `<app>/Contents/Resources`, so
 * the helper is its sibling under `Contents/Helpers`.
 */
export function tccHelperApp(resourcesPath = process.resourcesPath, platform: NodeJS.Platform = process.platform): string | undefined {
    if (platform !== 'darwin' || !resourcesPath) return undefined;
    return join(resourcesPath, '..', 'Helpers', 'BigSmoothTCC.app');
}

/** argv for `/usr/bin/open` that launches the TCC helper to drive one grant.
 * `-n` forces a fresh instance (the helper exits as soon as the prompt is
 * answered); `--args` forwards the rest to the helper's `smooth-daemon`. */
export function tccOpenArgs(helperApp: string, what: 'calendar' | 'reminders'): string[] {
    return ['-n', helperApp, '--args', 'tcc', what];
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
 * Ensure THIS Mac's own daemon is serving locally, spawning it if nothing already
 * answers `/health`. Always targets the LOCAL address ({@link localUrl}) — a remote
 * view target must never suppress the local agent, because the phone/relay and any
 * scheduled turns talk to the local daemon regardless of what the window shows
 * (th-5c2ec6). Returns how the daemon came to be so the caller can react.
 */
export async function startDaemon(): Promise<{ ok: boolean; spawned: boolean; error?: string }> {
    const url = localUrl();
    // Already up (a launchd/login-item instance, `th up`, or a prior spawn)? Attach.
    if (await isHealthy(url)) return { ok: true, spawned: false };

    const bin = resolveDaemonBin();
    if (!bin) {
        return { ok: false, spawned: false, error: `Could not find ${BIN}. Build it with \`pnpm install:th\`, or set SMOOTH_DAEMON_BIN.` };
    }

    // SMOOTH_MENUBAR=0: the bundled daemon lives in Contents/MacOS, which is the
    // daemon's own "I was launched as an app, show a status item" signal. We own
    // the tray, so turn its one off. stdio → desktop.log so a Finder-launched
    // daemon's startup errors aren't lost to a nonexistent terminal.
    desktopLog(`spawning daemon: ${bin} run (addr ${resolveAddr()})`);
    child = spawn(bin, ['run'], { stdio: daemonStdio(), env: { ...process.env, SMOOTH_ADDR: resolveAddr(), SMOOTH_MENUBAR: '0' } });
    child.on('exit', (code, signal) => {
        desktopLog(`daemon exited (code=${code ?? '?'} signal=${signal ?? '?'})`);
        child = undefined;
    });

    // ponytail: poll rather than parse the daemon's log for a ready line — /health
    // is the contract, and a 60s ceiling covers a cold sqlite migration on first run.
    const deadline = Date.now() + 60_000;
    while (Date.now() < deadline) {
        if (await isHealthy(url)) return { ok: true, spawned: true };
        if (!child) return { ok: false, spawned: true, error: `${BIN} exited during startup — see ${DESKTOP_LOG_FILE}.` };
        await new Promise((r) => setTimeout(r, 300));
    }
    return { ok: false, spawned: true, error: `${BIN} did not answer ${url}/health within 60s — see ${DESKTOP_LOG_FILE}.` };
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

//! Resolving, starting, and stopping the native `smooth-daemon` child.
//!
//! The daemon is the engine; this Electron app is only its shell. We attach to
//! an already-running daemon when one answers `/health` on the configured port
//! (the common case on a dev box running `th up` or a launchd unit) and only
//! spawn — and therefore only ever kill — a daemon we started ourselves.

import { type ChildProcess, execFile, execFileSync, spawn } from 'node:child_process';
import { appendFileSync, existsSync, mkdirSync, readFileSync } from 'node:fs';
import { homedir } from 'node:os';
import { delimiter, dirname, join } from 'node:path';

import { daemonLogDir, RotatingLog } from './daemonlog.js';
import {
    DEFAULT_POLICY,
    initialState,
    killAndWaitPolicy,
    reduce,
    type SupervisorAction,
    type SupervisorEvent,
    type SupervisorState,
    TailBuffer,
} from './supervisor.js';

const BIN = process.platform === 'win32' ? 'smooth-daemon.exe' : 'smooth-daemon';
const TH = process.platform === 'win32' ? 'th.exe' : 'th';

/** Where the running daemon advertises its bound `host:port` (written by
 * smooth-daemon's `persist_daemon_addr`). Lets us find it on whatever port it
 * landed on instead of guessing. */
const DAEMON_ADDR_FILE = join(homedir(), '.smooth', 'daemon.addr');

/** Where the desktop shell logs its own spawn/update diagnostics. Under
 * `open`/Finder there is no terminal, so a file is the only place a startup
 * error survives (th-5c2ec6). The daemon child's stdout/stderr and the
 * supervisor's lines go to {@link daemonLogPath} instead (th-4b189c). */
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

// ---- supervision (th-4b189c) -------------------------------------------------
//
// The app used to spawn the daemon once and only *note* its exit; a crash left
// the tray up and the port dead for hours. Now every child we own is watched:
// its stdout/stderr stream into a rotating daemon.log, its exit (with the last
// stderr lines) is logged and respawned with backoff, and a child that stops
// answering `/api/mode` is killed and treated the same. The decisions live in
// supervisor.ts (pure); this is the plumbing.

/** Probe cadence for a running child, and how long one probe may take. */
export const HEALTH_INTERVAL_MS = 30_000;
const HEALTH_TIMEOUT_MS = 5_000;
/** How many stderr lines an exit line carries. */
const STDERR_TAIL_LINES = 40;

let supState: SupervisorState = initialState();
let respawnTimer: NodeJS.Timeout | undefined;
let healthTimer: NodeJS.Timeout | undefined;
let daemonLogFile: RotatingLog | undefined;
const stderrTail = new TailBuffer(STDERR_TAIL_LINES);
const listeners = new Set<(state: SupervisorState) => void>();

/** Where this app's daemon logs live (`daemon.log` = child stdio + supervisor
 * lines; `smooth-daemon.log` = the daemon's own tracing via SMOOTH_LOG_FILE). */
export function daemonLogDirectory(): string {
    return daemonLogDir();
}

export function daemonLogPath(): string {
    return join(daemonLogDirectory(), 'daemon.log');
}

/** The file the daemon itself writes (passed as `SMOOTH_LOG_FILE`). */
export function daemonTracingLogPath(): string {
    return join(daemonLogDirectory(), 'smooth-daemon.log');
}

function daemonLog(): RotatingLog {
    daemonLogFile ??= new RotatingLog(daemonLogPath());
    return daemonLogFile;
}

/** Current supervision state (for the tray / About box). */
export function supervisorState(): SupervisorState {
    return supState;
}

/** Subscribe to supervision state changes. Returns an unsubscribe. */
export function onSupervisorChange(fn: (state: SupervisorState) => void): () => void {
    listeners.add(fn);
    return () => listeners.delete(fn);
}

/** `smooth-daemon --version` (cached) for the About box; undefined if unavailable. */
let cachedDaemonVersion: string | undefined;
export function daemonVersion(): string | undefined {
    if (cachedDaemonVersion !== undefined) return cachedDaemonVersion;
    const bin = resolveDaemonBin();
    if (!bin) return undefined;
    try {
        cachedDaemonVersion = execFileSync(bin, ['--version'], { encoding: 'utf8', timeout: 5_000, stdio: ['ignore', 'pipe', 'ignore'] })
            .trim()
            .replace(/^smooth-daemon\s+/, '');
    } catch {
        cachedDaemonVersion = undefined;
    }
    return cachedDaemonVersion;
}

/** Feed one event through the reducer, run its actions, notify listeners. */
function dispatch(event: SupervisorEvent): void {
    const { state, actions } = reduce(supState, event, DEFAULT_POLICY);
    supState = state;
    for (const action of actions) runAction(action);
    for (const fn of listeners) {
        try {
            fn(supState);
        } catch {
            // A tray repaint failure must not break supervision.
        }
    }
}

function runAction(action: SupervisorAction): void {
    switch (action.type) {
        case 'log':
            daemonLog().line(`[supervisor] ${action.line}`);
            desktopLog(`daemon supervisor: ${action.line}`);
            return;
        case 'respawn':
            clearTimeout(respawnTimer);
            respawnTimer = setTimeout(() => {
                respawnTimer = undefined;
                // The user may have quit, or clicked retry twice, in the meantime.
                if (supState.phase !== 'restarting' || child) return;
                spawnChild();
            }, action.delayMs);
            return;
        case 'kill': {
            const proc = child;
            if (!proc) return;
            // Its `exit` handler drives the respawn; SIGKILL fallback after the
            // same grace `stopDaemon` uses so a wedged daemon can't stall us.
            void killAndWait(proc, killAndWaitPolicy.graceMs);
            return;
        }
    }
}

/** How long a freshly spawned child gets to answer `/health` — covers a cold
 * sqlite migration on first run. */
const START_DEADLINE_MS = 60_000;

/** Spawn `smooth-daemon run` as our supervised child. Returns false if the
 * binary can't be found (the caller reports that; nothing to supervise). */
function spawnChild(): boolean {
    const bin = resolveDaemonBin();
    if (!bin) return false;
    const addr = resolveAddr();
    const log = daemonLog();
    stderrTail.clear();
    // SMOOTH_MENUBAR=0: the bundled daemon lives in Contents/MacOS, which is the
    // daemon's own "I was launched as an app, show a status item" signal. We own
    // the tray, so turn its one off. SMOOTH_LOG_FILE: the daemon's tracing goes
    // to its own rotating file; only panics/aborts reach stderr, which we capture
    // here so an exit line can quote them (th-4b189c).
    const env = { ...process.env, SMOOTH_ADDR: addr, SMOOTH_MENUBAR: '0', SMOOTH_LOG_FILE: daemonTracingLogPath() };
    desktopLog(`spawning daemon: ${bin} run (addr ${addr}, logs ${daemonLogDirectory()})`);
    let proc: ChildProcess;
    try {
        proc = spawn(bin, ['run'], { stdio: ['ignore', 'pipe', 'pipe'], env });
    } catch (err) {
        log.line(`[supervisor] spawn failed: ${String(err)}`);
        dispatch({ type: 'exited', code: null, signal: null, at: Date.now() });
        return true;
    }
    child = proc;
    proc.stdout?.on('data', (chunk: Buffer) => log.raw(chunk));
    proc.stderr?.on('data', (chunk: Buffer) => {
        log.raw(chunk);
        stderrTail.push(chunk);
    });
    proc.on('error', (err) => {
        // ENOENT/EACCES at spawn time: no `exit` follows, so surface it here.
        log.line(`[supervisor] child error: ${String(err)}`);
        if (proc.pid === undefined) {
            child = undefined;
            dispatch({ type: 'exited', code: null, signal: null, at: Date.now() });
        }
    });
    proc.on('exit', (code, signal) => {
        if (child === proc) child = undefined;
        const tail = stderrTail.tail();
        if (tail.length > 0) log.line(`[supervisor] last ${tail.length} stderr line(s) before exit:\n    ${tail.join('\n    ')}`);
        desktopLog(`daemon exited (code=${code ?? '?'} signal=${signal ?? '?'})`);
        dispatch({ type: 'exited', code, signal, at: Date.now() });
    });
    if (proc.pid !== undefined) {
        dispatch({ type: 'spawned', pid: proc.pid, at: Date.now() });
        void awaitReady(proc);
    }
    return true;
}

/**
 * Readiness poll for one spawned child: `/health` every 300ms until it answers
 * (→ `healthy`), the child dies (its exit handler takes over), or the deadline
 * passes (→ `start-timeout`, which kills it and backs off). Every spawn gets
 * one — a respawn that wedges at startup used to sit in `starting` forever,
 * where the 30s probe deliberately never kills (th-4b189c).
 */
async function awaitReady(proc: ChildProcess, deadlineMs = START_DEADLINE_MS): Promise<void> {
    const url = localUrl();
    const deadline = Date.now() + deadlineMs;
    while (Date.now() < deadline) {
        if (child !== proc || supState.phase !== 'starting') return; // died, stopped, or superseded
        if (await isHealthy(url)) {
            if (child === proc && supState.phase === 'starting') dispatch({ type: 'healthy', at: Date.now() });
            return;
        }
        await new Promise((r) => setTimeout(r, 300));
    }
    if (child === proc && supState.phase === 'starting') dispatch({ type: 'start-timeout', at: Date.now() });
}

/** `/api/mode` is the liveness probe (it exercises the axum router + a
 * spawn_blocking read, not just the TCP accept), so a daemon wedged inside
 * its runtime fails it even though the socket is still open. */
export async function probeMode(url = localUrl()): Promise<boolean> {
    try {
        const res = await fetch(`${url}/api/mode`, { signal: AbortSignal.timeout(HEALTH_TIMEOUT_MS) });
        return res.ok;
    } catch {
        return false;
    }
}

/** Start the periodic health probe (idempotent). Runs against the LOCAL daemon
 * whether we spawned it or attached — an attached daemon's death is reported in
 * the tray too; we just never kill or respawn what isn't ours. */
export function startHealthProbe(intervalMs = HEALTH_INTERVAL_MS): void {
    if (healthTimer) return;
    healthTimer = setInterval(() => void healthTick(), intervalMs);
    healthTimer.unref?.();
}

export async function healthTick(): Promise<void> {
    if (supState.phase !== 'running' && supState.phase !== 'attached' && supState.phase !== 'starting') return;
    const ok = await probeMode();
    dispatch({ type: ok ? 'healthy' : 'unhealthy', at: Date.now() });
}

/** The tray's "click to retry": clear the streak and respawn now. */
export function retryDaemon(): void {
    dispatch({ type: 'retry', at: Date.now() });
}

/**
 * Ensure THIS Mac's own daemon is serving locally, spawning it if nothing already
 * answers `/health`. Always targets the LOCAL address ({@link localUrl}) — a remote
 * view target must never suppress the local agent, because the phone/relay and any
 * scheduled turns talk to the local daemon regardless of what the window shows
 * (th-5c2ec6). Returns how the daemon came to be so the caller can react.
 *
 * A spawned child is supervised from here on (th-4b189c): a startup failure is
 * reported in the result AND handed to the supervisor, which keeps retrying with
 * backoff and reports through {@link onSupervisorChange}.
 */
export async function startDaemon(): Promise<{ ok: boolean; spawned: boolean; error?: string }> {
    const url = localUrl();
    // Already up (a launchd/login-item instance, `th up`, or a prior spawn)? Attach.
    if (await isHealthy(url)) {
        dispatch({ type: 'attached', at: Date.now() });
        startHealthProbe();
        return { ok: true, spawned: false };
    }

    if (!resolveDaemonBin()) {
        return { ok: false, spawned: false, error: `Could not find ${BIN}. Build it with \`pnpm install:th\`, or set SMOOTH_DAEMON_BIN.` };
    }
    // Move out of `idle` so the first spawn's exit is supervised rather than ignored.
    supState = { ...supState, phase: 'restarting', nextRetryAt: Date.now() };
    spawnChild();
    startHealthProbe();

    // ponytail: poll rather than parse the daemon's log for a ready line — /health
    // is the contract (awaitReady drives it); we just wait for the outcome.
    const deadline = Date.now() + START_DEADLINE_MS;
    while (Date.now() < deadline) {
        if (supState.phase === 'running') return { ok: true, spawned: true };
        if (supState.phase === 'stopped') return { ok: false, spawned: true, error: `${BIN} keeps exiting during startup — see ${daemonLogPath()}.` };
        await new Promise((r) => setTimeout(r, 300));
    }
    return { ok: false, spawned: true, error: `${BIN} did not answer ${url}/health within ${START_DEADLINE_MS / 1000}s — see ${daemonLogPath()}.` };
}

/** Terminate the daemon (only if we started it) and RESOLVE once it has actually
 * exited. Fire-and-forget SIGTERM used to race the OTA installer: `quitAndInstall`
 * handed the bundle to Squirrel while the daemon was still shutting down and
 * holding files inside `Big Smooth.app`, so the ditto/copy failed intermittently
 * ("couldn't copy bundle … no such file") and the update silently rolled back
 * (th-79416c). Awaiting the exit — with a SIGKILL fallback — frees the bundle
 * before the swap.
 *
 * `graceMs` is how long we wait for a clean SIGTERM exit before escalating to
 * SIGKILL; the returned promise always resolves (never rejects) so a quit path
 * can't hang on it. */
export async function stopDaemon(graceMs = killAndWaitPolicy.graceMs): Promise<void> {
    // Tell the supervisor this exit is ours, so it neither logs it as a crash
    // nor schedules a respawn into the bundle the updater is about to swap.
    if (child) daemonLog().line(`[supervisor] stopping pid ${child.pid ?? '?'} (app quit or update)`);
    dispatch({ type: 'stopping', at: Date.now() });
    clearTimeout(respawnTimer);
    respawnTimer = undefined;
    if (healthTimer) {
        clearInterval(healthTimer);
        healthTimer = undefined;
    }
    const proc = child;
    child = undefined;
    if (!proc || proc.exitCode !== null || proc.killed) return;
    await killAndWait(proc, graceMs);
    daemonLogFile?.close();
    daemonLogFile = undefined;
}

/** The minimal process surface `killAndWait` needs — lets it be unit-tested with
 * a fake instead of spawning a real daemon. */
export interface KillableProc {
    once(event: 'exit', listener: () => void): unknown;
    kill(signal: 'SIGTERM' | 'SIGKILL'): unknown;
}

/** SIGTERM `proc`, resolve once it emits `exit`, and escalate to SIGKILL if it
 * hasn't exited within `graceMs`. Always resolves (never rejects) so a quit path
 * can't hang. Extracted from `stopDaemon` so the OTA-critical logic is testable
 * (th-79416c). */
export async function killAndWait(proc: KillableProc, graceMs: number): Promise<void> {
    await new Promise<void>((resolve) => {
        let done = false;
        const finish = () => {
            if (done) return;
            done = true;
            clearTimeout(timer);
            resolve();
        };
        proc.once('exit', finish);
        proc.kill('SIGTERM');
        const timer = setTimeout(() => {
            try {
                proc.kill('SIGKILL');
            } catch {
                // already gone
            }
            finish();
        }, graceMs);
    });
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

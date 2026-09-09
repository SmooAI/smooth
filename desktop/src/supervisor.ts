//! Pure supervision logic for the bundled `smooth-daemon` child — no electron,
//! no child_process, so it runs under plain `node --test`. daemon.ts wires it to
//! the real process; main.ts renders it in the tray.
//!
//! Context (th-4b189c): Big Smooth.app ran for hours with its daemon child dead —
//! nothing on the port, no log of the exit anywhere — until the app was
//! relaunched. The `exit` handler only cleared a variable. This module decides
//! what happens instead: log the death, respawn with exponential backoff, give
//! up (visibly) after too many consecutive failures, and treat a child that is
//! alive but no longer answering `/api/mode` exactly like a dead one.

/** Backoff + give-up tuning. Attempts count CONSECUTIVE failures: a daemon that
 * stays healthy for `stableAfterMs` earns a clean slate, so a crash next week
 * starts again at `baseMs`, not at the cap. */
export interface SupervisorPolicy {
    /** Delay before the first respawn; doubles per consecutive failure. */
    baseMs: number;
    /** Ceiling on the doubling. */
    maxMs: number;
    /** Consecutive failed attempts before we stop respawning and ask the user. */
    maxAttempts: number;
    /** Healthy for this long ⇒ the failure streak resets to zero. */
    stableAfterMs: number;
    /** Consecutive failed health probes before a live child counts as hung. */
    hungAfterFailures: number;
}

/** 1s, 2s, 4s, 8s, 16s, 32s, 60s, 60s → about 3 minutes of trying, then stop. */
export const DEFAULT_POLICY: SupervisorPolicy = {
    baseMs: 1_000,
    maxMs: 60_000,
    maxAttempts: 8,
    stableAfterMs: 60_000,
    hungAfterFailures: 2,
};

/** Delay before respawn attempt number `attempt` (1-based): `base · 2^(n-1)`, capped. */
export function backoffMs(attempt: number, policy: SupervisorPolicy = DEFAULT_POLICY): number {
    const n = Math.max(1, Math.floor(attempt));
    // Cap the exponent so a runaway counter can't overflow to Infinity.
    return Math.min(policy.maxMs, policy.baseMs * 2 ** Math.min(n - 1, 30));
}

export type DaemonPhase =
    /** We spawned it and are waiting for `/health`. */
    | 'starting'
    /** Answering probes. */
    | 'running'
    /** Died (or hung and was killed); a respawn is scheduled. */
    | 'restarting'
    /** Gave up after `maxAttempts` consecutive failures — the user must retry. */
    | 'stopped'
    /** Not our child: a `th up` / launchd daemon we attached to. Never respawned. */
    | 'attached'
    /** No child and none expected (before start, or after a deliberate stop). */
    | 'idle';

export interface LastExit {
    code: number | null;
    signal: string | null;
    at: number;
    /** Why we're treating this as an exit: the process died, or we killed a hung one. */
    reason: 'exited' | 'hung';
}

export interface SupervisorState {
    phase: DaemonPhase;
    pid?: number;
    /** When the current child was spawned (ms epoch). */
    startedAt?: number;
    /** Lifetime respawn count for this app session — what the About box shows. */
    restarts: number;
    /** Consecutive failures in the current streak (drives backoff + give-up). */
    attempt: number;
    /** Consecutive failed health probes for the current child. */
    healthFailures: number;
    lastExit?: LastExit;
    /** When the pending respawn fires (ms epoch), while `restarting`. */
    nextRetryAt?: number;
}

export function initialState(): SupervisorState {
    return { phase: 'idle', restarts: 0, attempt: 0, healthFailures: 0 };
}

export type SupervisorEvent =
    | { type: 'attached'; at: number }
    | { type: 'spawned'; pid: number; at: number }
    | { type: 'healthy'; at: number }
    | { type: 'unhealthy'; at: number }
    /** The child never answered `/health` within the startup deadline. */
    | { type: 'start-timeout'; at: number }
    | { type: 'exited'; code: number | null; signal: string | null; at: number }
    /** The user clicked "retry" (tray) — clears the streak and respawns now. */
    | { type: 'retry'; at: number }
    /** We are quitting / handing the bundle to the updater: never respawn again. */
    | { type: 'stopping'; at: number };

export type SupervisorAction =
    /** Spawn a new child after `delayMs`. */
    | { type: 'respawn'; delayMs: number }
    /** The child is alive but hung — kill it (its `exit` then drives the respawn). */
    | { type: 'kill' }
    /** Something worth a line in daemon.log. */
    | { type: 'log'; line: string };

export interface Transition {
    state: SupervisorState;
    actions: SupervisorAction[];
}

/** The state machine. Pure: same state + event ⇒ same transition. */
export function reduce(state: SupervisorState, event: SupervisorEvent, policy: SupervisorPolicy = DEFAULT_POLICY): Transition {
    switch (event.type) {
        case 'attached':
            return { state: { ...state, phase: 'attached', pid: undefined, startedAt: event.at, healthFailures: 0, nextRetryAt: undefined }, actions: [] };

        case 'spawned':
            return {
                state: { ...state, phase: 'starting', pid: event.pid, startedAt: event.at, healthFailures: 0, nextRetryAt: undefined },
                actions: [{ type: 'log', line: `spawned pid ${event.pid}${state.attempt > 0 ? ` (attempt ${state.attempt}/${policy.maxAttempts})` : ''}` }],
            };

        case 'healthy': {
            if (state.phase !== 'starting' && state.phase !== 'running' && state.phase !== 'attached') return { state, actions: [] };
            const actions: SupervisorAction[] = [];
            let attempt = state.attempt;
            if (state.phase === 'starting') actions.push({ type: 'log', line: `healthy pid ${state.pid ?? '?'}` });
            // Stable long enough ⇒ the streak is over; the next crash starts small again.
            if (attempt > 0 && state.startedAt !== undefined && event.at - state.startedAt >= policy.stableAfterMs) {
                attempt = 0;
                actions.push({ type: 'log', line: `stable for ${formatDuration(event.at - state.startedAt)} — failure streak reset` });
            }
            const phase = state.phase === 'attached' ? 'attached' : 'running';
            return { state: { ...state, phase, attempt, healthFailures: 0 }, actions };
        }

        case 'unhealthy': {
            // Only a child we own and believe is up can be "hung". A failed probe
            // during `starting` is just "not ready yet" — startDaemon's own deadline
            // covers that; an attached daemon isn't ours to kill.
            if (state.phase !== 'running') return { state, actions: [] };
            const healthFailures = state.healthFailures + 1;
            if (healthFailures < policy.hungAfterFailures) {
                return {
                    state: { ...state, healthFailures },
                    actions: [{ type: 'log', line: `health probe failed (${healthFailures}/${policy.hungAfterFailures})` }],
                };
            }
            // Alive but not answering: same treatment as dead. Kill it; the resulting
            // `exited` event schedules the respawn through the normal backoff.
            return {
                state: { ...state, phase: 'restarting', healthFailures, lastExit: { code: null, signal: null, at: event.at, reason: 'hung' } },
                actions: [{ type: 'log', line: `pid ${state.pid ?? '?'} hung — ${healthFailures} consecutive probes failed, killing it` }, { type: 'kill' }],
            };
        }

        case 'start-timeout': {
            // Alive but never came up. Same treatment as hung: kill it, and let
            // the exit drive the backoff so a daemon that wedges at startup every
            // time still ends in `stopped` rather than a silent forever-starting.
            if (state.phase !== 'starting') return { state, actions: [] };
            return {
                state: { ...state, phase: 'restarting', lastExit: { code: null, signal: null, at: event.at, reason: 'hung' } },
                actions: [{ type: 'log', line: `pid ${state.pid ?? '?'} never answered /health — killing it` }, { type: 'kill' }],
            };
        }

        case 'exited': {
            // Deliberate stop, or a child that was never ours: nothing to do.
            if (state.phase === 'idle' || state.phase === 'attached' || state.phase === 'stopped') return { state, actions: [] };
            const hung = state.lastExit?.reason === 'hung' && state.phase === 'restarting';
            const lastExit: LastExit = { code: event.code, signal: event.signal, at: event.at, reason: hung ? 'hung' : 'exited' };
            const attempt = state.attempt + 1;
            const uptime = state.startedAt === undefined ? undefined : event.at - state.startedAt;
            const why = `pid ${state.pid ?? '?'} ${hung ? 'killed (hung)' : 'exited'} code=${event.code ?? '?'} signal=${event.signal ?? '?'}${uptime === undefined ? '' : ` after ${formatDuration(uptime)}`}`;
            if (attempt > policy.maxAttempts) {
                return {
                    state: { ...state, phase: 'stopped', pid: undefined, attempt, lastExit, nextRetryAt: undefined },
                    actions: [{ type: 'log', line: `${why} — giving up after ${policy.maxAttempts} consecutive failures; waiting for the user` }],
                };
            }
            const delayMs = backoffMs(attempt, policy);
            return {
                state: { ...state, phase: 'restarting', pid: undefined, attempt, restarts: state.restarts + 1, lastExit, nextRetryAt: event.at + delayMs },
                actions: [
                    { type: 'log', line: `${why} — respawning in ${formatDuration(delayMs)} (attempt ${attempt}/${policy.maxAttempts})` },
                    { type: 'respawn', delayMs },
                ],
            };
        }

        case 'retry':
            if (state.phase === 'attached' || state.phase === 'running' || state.phase === 'starting') return { state, actions: [] };
            return {
                state: {
                    ...state,
                    phase: 'restarting',
                    attempt: 0,
                    healthFailures: 0,
                    nextRetryAt: event.at,
                    restarts: state.phase === 'stopped' ? state.restarts + 1 : state.restarts,
                },
                actions: [
                    { type: 'log', line: 'user asked for a retry — respawning now' },
                    { type: 'respawn', delayMs: 0 },
                ],
            };

        case 'stopping':
            return { state: { ...state, phase: 'idle', nextRetryAt: undefined }, actions: [] };
    }
}

/** Keep the last `capacity` lines of a byte stream (the child's stderr) so an
 * exit line can carry them. Handles chunks that split mid-line. */
export class TailBuffer {
    private lines: string[] = [];
    private partial = '';

    constructor(private readonly capacity = 40) {}

    push(chunk: string | Uint8Array): void {
        const text = typeof chunk === 'string' ? chunk : Buffer.from(chunk).toString('utf8');
        const parts = (this.partial + text).split(/\r?\n/);
        this.partial = parts.pop() ?? '';
        for (const line of parts) {
            if (line.trim() === '') continue;
            this.lines.push(line);
            if (this.lines.length > this.capacity) this.lines.shift();
        }
    }

    /** Last N lines, oldest first (including a trailing unterminated line). */
    tail(n = this.capacity): string[] {
        const all = this.partial.trim() === '' ? this.lines : [...this.lines, this.partial];
        return all.slice(Math.max(0, all.length - n));
    }

    clear(): void {
        this.lines = [];
        this.partial = '';
    }
}

/** `1h 2m`, `45s`, `800ms` — for tray labels and log lines. */
export function formatDuration(ms: number): string {
    if (!Number.isFinite(ms) || ms < 0) return '0s';
    if (ms < 1_000) return `${Math.round(ms)}ms`;
    const s = Math.floor(ms / 1_000);
    if (s < 60) return `${s}s`;
    const m = Math.floor(s / 60);
    if (m < 60) return `${m}m${s % 60 ? ` ${s % 60}s` : ''}`;
    const h = Math.floor(m / 60);
    if (h < 24) return `${h}h${m % 60 ? ` ${m % 60}m` : ''}`;
    const d = Math.floor(h / 24);
    return `${d}d${h % 24 ? ` ${h % 24}h` : ''}`;
}

/** One-line daemon status for the tray. Presence tone: quiet when he's here,
 * plain words (no jargon) when he isn't, and the only line that ever asks the
 * user to do something is the give-up one. */
export function trayDaemonLabel(state: SupervisorState, now: number, policy: SupervisorPolicy = DEFAULT_POLICY): string {
    switch (state.phase) {
        case 'attached':
            return 'Daemon: running (started outside the app)';
        case 'starting':
            return 'Daemon: starting…';
        case 'running': {
            const up = state.startedAt === undefined ? '' : ` · up ${formatDuration(now - state.startedAt)}`;
            const restarts = state.restarts === 0 ? '' : ` · ${state.restarts} restart${state.restarts === 1 ? '' : 's'}`;
            return `Daemon: running${up}${restarts}`;
        }
        case 'restarting': {
            const inMs = state.nextRetryAt === undefined ? 0 : Math.max(0, state.nextRetryAt - now);
            const when = inMs < 1_000 ? 'now' : `in ${formatDuration(inMs)}`;
            return `Daemon crashed — restarting ${when} (attempt ${state.attempt}/${policy.maxAttempts})`;
        }
        case 'stopped':
            return 'Daemon stopped — click to retry';
        case 'idle':
            return 'Daemon: not running';
    }
}

/** Whether the tray status line should be clickable (retry). */
export function trayDaemonClickable(state: SupervisorState): boolean {
    return state.phase === 'stopped' || state.phase === 'restarting';
}

/** The About box body. `port` and `logDir` come from the caller (daemon.ts). */
export function aboutDaemonDetail(opts: {
    state: SupervisorState;
    now: number;
    port: string;
    logDir: string;
    appVersion: string;
    daemonVersion?: string;
}): string {
    const { state, now } = opts;
    const lines = [`Big Smooth ${opts.appVersion}`, `smooth-daemon ${opts.daemonVersion ?? '(version unknown)'}`, ''];
    lines.push(`Status: ${trayDaemonLabel(state, now)}`);
    lines.push(`Address: ${opts.port}`);
    lines.push(`PID: ${state.pid ?? '—'}`);
    lines.push(
        `Uptime: ${state.startedAt === undefined || state.phase === 'stopped' || state.phase === 'restarting' ? '—' : formatDuration(now - state.startedAt)}`,
    );
    lines.push(`Restarts this session: ${state.restarts}`);
    if (state.lastExit) {
        const e = state.lastExit;
        lines.push(
            `Last exit: ${e.reason === 'hung' ? 'hung, killed' : 'exited'} code=${e.code ?? '?'} signal=${e.signal ?? '?'} ${formatDuration(now - e.at)} ago`,
        );
    }
    lines.push('', `Logs: ${opts.logDir}`);
    return lines.join('\n');
}

/** Shared SIGTERM grace before SIGKILL — used by both the OTA stop path and the
 * hung-child kill so the two can't drift apart. */
export const killAndWaitPolicy = { graceMs: 4_000 };

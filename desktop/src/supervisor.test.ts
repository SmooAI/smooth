import assert from 'node:assert/strict';
import { describe, it } from 'node:test';

import {
    aboutDaemonDetail,
    backoffMs,
    DEFAULT_POLICY,
    formatDuration,
    initialState,
    reduce,
    type SupervisorAction,
    type SupervisorPolicy,
    type SupervisorState,
    TailBuffer,
    trayDaemonClickable,
    trayDaemonLabel,
} from './supervisor.js';

const policy: SupervisorPolicy = { baseMs: 1_000, maxMs: 8_000, maxAttempts: 3, stableAfterMs: 60_000, hungAfterFailures: 2 };

const only = (actions: SupervisorAction[], type: SupervisorAction['type']) => actions.filter((a) => a.type === type);

/** Spawn + become healthy at t=0 — the common starting point. */
function running(): SupervisorState {
    let s = initialState();
    s = reduce(s, { type: 'spawned', pid: 100, at: 0 }, policy).state;
    s = reduce(s, { type: 'healthy', at: 100 }, policy).state;
    return s;
}

describe('backoffMs', () => {
    it('doubles from the base and caps', () => {
        assert.equal(backoffMs(1, policy), 1_000);
        assert.equal(backoffMs(2, policy), 2_000);
        assert.equal(backoffMs(3, policy), 4_000);
        assert.equal(backoffMs(4, policy), 8_000);
        assert.equal(backoffMs(5, policy), 8_000);
        assert.equal(backoffMs(500, policy), 8_000, 'a runaway counter still caps (no Infinity)');
    });

    it('treats attempt < 1 as the first attempt', () => {
        assert.equal(backoffMs(0, policy), 1_000);
        assert.equal(backoffMs(-3, policy), 1_000);
    });

    it('default policy spends about three minutes before giving up', () => {
        let total = 0;
        for (let n = 1; n <= DEFAULT_POLICY.maxAttempts; n++) total += backoffMs(n);
        assert.ok(total >= 150_000 && total <= 240_000, `total backoff ${total}ms`);
    });
});

describe('reduce: the crash → respawn → give-up path', () => {
    it('spawned → starting, healthy → running', () => {
        const t1 = reduce(initialState(), { type: 'spawned', pid: 42, at: 0 }, policy);
        assert.equal(t1.state.phase, 'starting');
        assert.equal(t1.state.pid, 42);
        assert.match(only(t1.actions, 'log')[0]!.type === 'log' ? (t1.actions[0] as { line: string }).line : '', /spawned pid 42/);
        const t2 = reduce(t1.state, { type: 'healthy', at: 100 }, policy);
        assert.equal(t2.state.phase, 'running');
        assert.equal(t2.state.healthFailures, 0);
    });

    it('an exit schedules a respawn with backoff and logs code/signal', () => {
        const t = reduce(running(), { type: 'exited', code: null, signal: 'SIGKILL', at: 5_000 }, policy);
        assert.equal(t.state.phase, 'restarting');
        assert.equal(t.state.pid, undefined);
        assert.equal(t.state.attempt, 1);
        assert.equal(t.state.restarts, 1);
        assert.equal(t.state.nextRetryAt, 6_000);
        assert.deepEqual(only(t.actions, 'respawn'), [{ type: 'respawn', delayMs: 1_000 }]);
        const line = (only(t.actions, 'log')[0] as { line: string }).line;
        assert.match(line, /pid 100 exited code=\? signal=SIGKILL after 5s/);
        assert.match(line, /respawning in 1s \(attempt 1\/3\)/);
        assert.deepEqual(t.state.lastExit, { code: null, signal: 'SIGKILL', at: 5_000, reason: 'exited' });
    });

    it('consecutive crashes back off exponentially, then give up visibly', () => {
        let s = running();
        const delays: number[] = [];
        for (let i = 0; i < policy.maxAttempts; i++) {
            const t = reduce(s, { type: 'exited', code: 1, signal: null, at: 1_000 * i }, policy);
            assert.equal(t.state.phase, 'restarting');
            delays.push((only(t.actions, 'respawn')[0] as { delayMs: number }).delayMs);
            s = reduce(t.state, { type: 'spawned', pid: 200 + i, at: 1_000 * i + 10 }, policy).state;
            // It comes up briefly (under stableAfterMs) then dies again.
            s = reduce(s, { type: 'healthy', at: 1_000 * i + 20 }, policy).state;
            assert.equal(s.attempt, i + 1, 'a short-lived healthy period does not reset the streak');
        }
        assert.deepEqual(delays, [1_000, 2_000, 4_000]);
        // One more than maxAttempts → stopped, no respawn.
        const t = reduce(s, { type: 'exited', code: 1, signal: null, at: 99_000 }, policy);
        assert.equal(t.state.phase, 'stopped');
        assert.equal(only(t.actions, 'respawn').length, 0);
        assert.match((only(t.actions, 'log')[0] as { line: string }).line, /giving up after 3 consecutive failures/);
        assert.equal(t.state.restarts, 3, 'the give-up itself is not counted as a restart');
    });

    it('a long stable run resets the failure streak', () => {
        let s = running();
        s = reduce(s, { type: 'exited', code: 1, signal: null, at: 1_000 }, policy).state;
        s = reduce(s, { type: 'spawned', pid: 101, at: 2_000 }, policy).state;
        s = reduce(s, { type: 'healthy', at: 2_100 }, policy).state;
        assert.equal(s.attempt, 1);
        // A probe a minute later finds it still healthy → clean slate.
        const t = reduce(s, { type: 'healthy', at: 2_000 + policy.stableAfterMs }, policy);
        assert.equal(t.state.attempt, 0);
        assert.match((only(t.actions, 'log')[0] as { line: string }).line, /stable for 1m — failure streak reset/);
        // …so the next crash waits only the base delay again.
        const t2 = reduce(t.state, { type: 'exited', code: 1, signal: null, at: 70_000 }, policy);
        assert.deepEqual(only(t2.actions, 'respawn'), [{ type: 'respawn', delayMs: 1_000 }]);
        assert.equal(t2.state.restarts, 2, 'lifetime restart count keeps climbing');
    });

    it('retry from stopped clears the streak and respawns immediately', () => {
        let s: SupervisorState = { ...running(), phase: 'stopped', pid: undefined, attempt: 4, restarts: 3 };
        const t = reduce(s, { type: 'retry', at: 10 }, policy);
        assert.equal(t.state.phase, 'restarting');
        assert.equal(t.state.attempt, 0);
        assert.equal(t.state.restarts, 4);
        assert.deepEqual(only(t.actions, 'respawn'), [{ type: 'respawn', delayMs: 0 }]);
        // Retry while already running/starting/attached is a no-op.
        s = running();
        assert.deepEqual(reduce(s, { type: 'retry', at: 10 }, policy), { state: s, actions: [] });
    });

    it('retry while restarting (waiting on backoff) respawns now without double-counting', () => {
        const s = reduce(running(), { type: 'exited', code: 1, signal: null, at: 1_000 }, policy).state;
        const t = reduce(s, { type: 'retry', at: 1_500 }, policy);
        assert.equal(t.state.phase, 'restarting');
        assert.equal(t.state.restarts, 1, 'the crash already counted; the retry does not add one');
        assert.deepEqual(only(t.actions, 'respawn'), [{ type: 'respawn', delayMs: 0 }]);
    });
});

describe('reduce: hung children', () => {
    it('one failed probe is only logged; the threshold kills', () => {
        const t1 = reduce(running(), { type: 'unhealthy', at: 30_000 }, policy);
        assert.equal(t1.state.phase, 'running');
        assert.equal(t1.state.healthFailures, 1);
        assert.equal(only(t1.actions, 'kill').length, 0);
        const t2 = reduce(t1.state, { type: 'unhealthy', at: 60_000 }, policy);
        assert.equal(t2.state.phase, 'restarting');
        assert.deepEqual(only(t2.actions, 'kill'), [{ type: 'kill' }]);
        assert.match((only(t2.actions, 'log')[0] as { line: string }).line, /pid 100 hung — 2 consecutive probes failed, killing it/);
        // The kill's exit then respawns with backoff and remembers WHY.
        const t3 = reduce(t2.state, { type: 'exited', code: null, signal: 'SIGKILL', at: 61_000 }, policy);
        assert.equal(t3.state.phase, 'restarting');
        assert.equal(t3.state.lastExit?.reason, 'hung');
        assert.match((only(t3.actions, 'log')[0] as { line: string }).line, /killed \(hung\)/);
        assert.deepEqual(only(t3.actions, 'respawn'), [{ type: 'respawn', delayMs: 1_000 }]);
    });

    it('a healthy probe clears the failure count', () => {
        const t1 = reduce(running(), { type: 'unhealthy', at: 30_000 }, policy);
        const t2 = reduce(t1.state, { type: 'healthy', at: 60_000 }, policy);
        assert.equal(t2.state.healthFailures, 0);
        assert.equal(t2.state.phase, 'running');
    });

    it('a child that never comes up is killed on start-timeout and then backed off', () => {
        const starting = reduce(initialState(), { type: 'spawned', pid: 7, at: 0 }, policy).state;
        const t = reduce(starting, { type: 'start-timeout', at: 60_000 }, policy);
        assert.equal(t.state.phase, 'restarting');
        assert.deepEqual(only(t.actions, 'kill'), [{ type: 'kill' }]);
        assert.match((only(t.actions, 'log')[0] as { line: string }).line, /pid 7 never answered \/health — killing it/);
        const t2 = reduce(t.state, { type: 'exited', code: null, signal: 'SIGKILL', at: 64_000 }, policy);
        assert.equal(t2.state.lastExit?.reason, 'hung');
        assert.deepEqual(only(t2.actions, 'respawn'), [{ type: 'respawn', delayMs: 1_000 }]);
        // Outside `starting` the event is meaningless.
        assert.deepEqual(reduce(running(), { type: 'start-timeout', at: 1 }, policy), { state: running(), actions: [] });
    });

    it('probes during starting or attached never kill', () => {
        const starting = reduce(initialState(), { type: 'spawned', pid: 7, at: 0 }, policy).state;
        for (let i = 0; i < 5; i++) {
            const t = reduce(starting, { type: 'unhealthy', at: i }, policy);
            assert.deepEqual(t, { state: starting, actions: [] });
        }
        const attached = reduce(initialState(), { type: 'attached', at: 0 }, policy).state;
        assert.equal(attached.phase, 'attached');
        assert.deepEqual(reduce(attached, { type: 'unhealthy', at: 1 }, policy), { state: attached, actions: [] });
    });
});

describe('reduce: exits we must NOT react to', () => {
    it('after stopping (quit / OTA swap) an exit is ignored', () => {
        const s = reduce(running(), { type: 'stopping', at: 1 }, policy).state;
        assert.equal(s.phase, 'idle');
        const t = reduce(s, { type: 'exited', code: 0, signal: null, at: 2 }, policy);
        assert.deepEqual(t, { state: s, actions: [] });
    });

    it('an attached daemon is never respawned', () => {
        const s = reduce(initialState(), { type: 'attached', at: 0 }, policy).state;
        assert.deepEqual(reduce(s, { type: 'exited', code: 1, signal: null, at: 1 }, policy), { state: s, actions: [] });
    });

    it('once stopped, further exits do not resurrect a respawn', () => {
        const s: SupervisorState = { ...initialState(), phase: 'stopped', attempt: 9 };
        assert.deepEqual(reduce(s, { type: 'exited', code: 1, signal: null, at: 1 }, policy), { state: s, actions: [] });
    });
});

describe('TailBuffer', () => {
    it('keeps the last N lines across split chunks', () => {
        const t = new TailBuffer(3);
        t.push('a\nb\nc');
        t.push('c-more\nd\n');
        assert.deepEqual(t.tail(), ['b', 'cc-more', 'd']);
        t.push('e');
        assert.deepEqual(t.tail(), ['cc-more', 'd', 'e'], 'a trailing unterminated line is included');
        assert.deepEqual(t.tail(2), ['d', 'e']);
    });

    it('accepts bytes and skips blank lines', () => {
        const t = new TailBuffer(5);
        t.push(Buffer.from('x\n\n   \ny\n'));
        assert.deepEqual(t.tail(), ['x', 'y']);
        t.clear();
        assert.deepEqual(t.tail(), []);
    });
});

describe('formatDuration', () => {
    it('picks a human unit', () => {
        assert.equal(formatDuration(500), '500ms');
        assert.equal(formatDuration(45_000), '45s');
        assert.equal(formatDuration(60_000), '1m');
        assert.equal(formatDuration(125_000), '2m 5s');
        assert.equal(formatDuration(3_600_000), '1h');
        assert.equal(formatDuration(7_380_000), '2h 3m');
        assert.equal(formatDuration(90_000_000), '1d 1h');
        assert.equal(formatDuration(-5), '0s');
        assert.equal(formatDuration(Number.NaN), '0s');
    });
});

describe('tray labels', () => {
    it('running shows uptime and restarts; the rest say what is happening', () => {
        const s = { ...running(), restarts: 1 };
        assert.equal(trayDaemonLabel(s, 3_600_000), 'Daemon: running · up 1h · 1 restart');
        assert.equal(trayDaemonLabel({ ...s, restarts: 0 }, 60_000), 'Daemon: running · up 1m');
        assert.equal(trayDaemonLabel(reduce(initialState(), { type: 'spawned', pid: 1, at: 0 }, policy).state, 0), 'Daemon: starting…');
        assert.equal(trayDaemonLabel(reduce(initialState(), { type: 'attached', at: 0 }, policy).state, 0), 'Daemon: running (started outside the app)');
        assert.equal(trayDaemonLabel(initialState(), 0), 'Daemon: not running');
    });

    it('restarting counts down; stopped asks for a click', () => {
        const s = reduce(running(), { type: 'exited', code: 1, signal: null, at: 1_000 }, policy).state;
        assert.equal(trayDaemonLabel(s, 1_000, policy), 'Daemon crashed — restarting in 1s (attempt 1/3)');
        assert.equal(trayDaemonLabel(s, 1_900, policy), 'Daemon crashed — restarting now (attempt 1/3)');
        assert.equal(trayDaemonClickable(s), true);
        const stopped: SupervisorState = { ...s, phase: 'stopped' };
        assert.equal(trayDaemonLabel(stopped, 0), 'Daemon stopped — click to retry');
        assert.equal(trayDaemonClickable(stopped), true);
        assert.equal(trayDaemonClickable(running()), false);
    });
});

describe('aboutDaemonDetail', () => {
    it('lists pid, address, uptime, restarts, last exit and the log dir', () => {
        let s = running();
        s = reduce(s, { type: 'exited', code: 137, signal: null, at: 10_000 }, policy).state;
        s = reduce(s, { type: 'spawned', pid: 555, at: 11_000 }, policy).state;
        s = reduce(s, { type: 'healthy', at: 11_100 }, policy).state;
        const text = aboutDaemonDetail({
            state: s,
            now: 71_000,
            port: '127.0.0.1:8899',
            logDir: '/Users/x/Library/Logs/Big Smooth',
            appVersion: '0.1.13',
            daemonVersion: '0.44.0 (abc1234)',
        });
        assert.match(text, /^Big Smooth 0\.1\.13\nsmooth-daemon 0\.44\.0 \(abc1234\)/);
        assert.match(text, /Address: 127\.0\.0\.1:8899/);
        assert.match(text, /PID: 555/);
        assert.match(text, /Uptime: 1m/);
        assert.match(text, /Restarts this session: 1/);
        assert.match(text, /Last exit: exited code=137 signal=\? 1m 1s ago/);
        assert.match(text, /Logs: \/Users\/x\/Library\/Logs\/Big Smooth/);
    });

    it('shows dashes when there is no child', () => {
        const text = aboutDaemonDetail({ state: initialState(), now: 0, port: '127.0.0.1:8787', logDir: '/l', appVersion: '0.1.13' });
        assert.match(text, /smooth-daemon \(version unknown\)/);
        assert.match(text, /PID: —/);
        assert.match(text, /Uptime: —/);
        assert.doesNotMatch(text, /Last exit/);
    });
});

import assert from 'node:assert/strict';
import { test } from 'node:test';

import {
    appendSamples,
    DemoQueue,
    diff,
    duration,
    gaugeFill,
    GATE_X,
    heatOf,
    labelP50s,
    lanes,
    lockHolders,
    machineHeat,
    progress,
    shortLock,
    signals,
    waitReason,
    type HistoryEntry,
    type JobInfo,
    type Snapshot,
} from './ci-queue.ts';

const GB = 1_073_741_824;

function snap(over: Partial<Snapshot> = {}): Snapshot {
    return {
        schema: 1,
        dir: '/q',
        config: {
            slots: { heavy: 2, light: 6 },
            gate: { min_available_memory_pct: 5, max_memory_pressure_level: 1, max_swap_used_pct: 90, max_load_per_core: 4, min_free_disk_gb: 20 },
        },
        now_ms: 100_000,
        running: [],
        waiting: [],
        readings: {
            mem_total_bytes: 24 * GB,
            mem_available_bytes: 12 * GB,
            memory_pressure_level: 1,
            swap_total_bytes: 20 * GB,
            swap_used_bytes: 2 * GB,
            load1: 12,
            cores: 12,
            disks: [{ path: '/', free_bytes: 200 * GB }],
        },
        holds: [],
        history: [],
        ...over,
    };
}

function job(ticket: number, over: Partial<JobInfo> = {}): JobInfo {
    return { class: 'heavy', ticket, label: `job ${ticket}`, pid: ticket, cwd: '/w/repo', queued_at_ms: 0, ...over };
}

function hist(label: string, run_ms: number, over: Partial<HistoryEntry> = {}): HistoryEntry {
    return { label, class: 'heavy', cwd: '/w', ticket: 1, queued_at_ms: 0, wait_ms: 0, run_ms, outcome: 'exit', exit: 0, ...over };
}

test('heat walks the spectrum as a signal nears and passes its threshold', () => {
    assert.equal(heatOf(0), 0);
    assert.equal(heatOf(0.6), 1);
    assert.equal(heatOf(0.8), 2);
    assert.equal(heatOf(0.95), 3);
    assert.equal(heatOf(1.05), 4);
    assert.equal(heatOf(2), 5);
    assert.equal(heatOf(Number.NaN), 0);
});

test('every gauge puts its threshold at the same x, and past it is past it', () => {
    assert.equal(gaugeFill(1), GATE_X);
    assert.ok(gaugeFill(1.1) > GATE_X);
    assert.equal(gaugeFill(10), 1);
    assert.equal(gaugeFill(null), 0);
});

test('signals read each gate threshold in the worse-is-higher direction', () => {
    const s = signals(snap().readings, snap().config.gate);
    const by = Object.fromEntries(s.map((x) => [x.key, x]));
    assert.equal(by.memory.value, '50% used');
    assert.ok(Math.abs((by.memory.ratio ?? 0) - 50 / 95) < 1e-9);
    assert.equal(by.swap.value, '10% used');
    assert.equal(by.load.value, '12 · 1.0/core');
    assert.equal(by.load.ratio, 0.25);
    assert.equal(by.disk.value, '200 GB free');
    assert.equal(by.disk.ratio, 0.1);
    assert.equal(by.pressure.value, 'normal');
});

test('an unknown reading is unknown, not zero, and a 0 threshold turns a signal off', () => {
    const r = { cores: 12, disks: [] };
    const g = { ...snap().config.gate, max_load_per_core: 0 };
    const s = signals(r, g);
    assert.ok(s.every((x) => x.ratio === null));
    assert.ok(s.every((x) => x.value === 'unknown'));
    assert.equal(s.find((x) => x.key === 'load')?.off, true);
    assert.equal(
        s.find((x) => x.key === 'pressure'),
        undefined,
        'no pressure row off macOS',
    );
});

test('machine heat follows the gate: calm when nothing holds, hot when something does', () => {
    assert.equal(machineHeat(snap()), 1);
    // load past threshold but the gate says clear (e.g. no heavy job running) → capped at gold
    const busy = snap({ readings: { ...snap().readings, load1: 12 * 6 } });
    assert.equal(machineHeat(busy), 3);
    assert.equal(machineHeat({ ...busy, holds: ['load 6.0/core > 4/core'] }), 5);
    // a hold with cool readings is still at least orange
    assert.equal(machineHeat(snap({ holds: ['disk'] })), 4);
});

test('full swap on its own does not heat the machine — only while memory is tight', () => {
    const swapFull = snap({ readings: { ...snap().readings, swap_used_bytes: 19.5 * GB } });
    assert.equal(machineHeat(swapFull), 1);
    const tight = { ...swapFull, readings: { ...swapFull.readings, memory_pressure_level: 2 } };
    assert.equal(machineHeat(tight), 3);
});

test('p50 per label ignores jobs that never ran', () => {
    const p = labelP50s([
        hist('clippy', 10_000),
        hist('clippy', 30_000),
        hist('clippy', 20_000),
        hist('tsc', 4_000),
        hist('tsc', 6_000),
        hist('tsc', 0, { outcome: 'wait-timeout' }),
        hist('never', 5_000, { outcome: 'spawn-failed' }),
    ]);
    assert.equal(p.get('clippy'), 20_000);
    assert.equal(p.get('tsc'), 5_000);
    assert.equal(p.has('never'), false);
});

test('progress is elapsed vs the label p50, indeterminate without one', () => {
    const p50 = new Map([['clippy', 60_000]]);
    const j = job(1, { label: 'clippy', admitted_at_ms: 40_000 });
    assert.deepEqual(progress(j, 70_000, p50), { elapsedMs: 30_000, frac: 0.5, over: false });
    assert.deepEqual(progress(j, 200_000, p50), { elapsedMs: 160_000, frac: 1, over: true });
    assert.equal(progress(job(2, { label: 'new', admitted_at_ms: 0 }), 5_000, p50).frac, null);
});

test('lanes put each job in its slot and keep free slots as gaps', () => {
    // The queue numbers slots from 1: heavy-2 is the second lane.
    const s = snap({ running: [job(1, { slot: 2 }), job(2, { class: 'light', slot: 1 })] });
    const heavy = lanes(s, 'heavy');
    assert.equal(heavy.length, 2);
    assert.equal(heavy[0], null);
    assert.equal(heavy[1]?.ticket, 1);
    assert.equal(lanes(s, 'light')[0]?.ticket, 2);
    // a job in a slot beyond the configured count still shows
    const shrunk = snap({ running: [job(3, { slot: 5 })] });
    assert.equal(lanes(shrunk, 'heavy').length, 3);
});

test("wait reasons prefer the queue's own words and fall back sensibly", () => {
    const s = snap({ running: [job(1, { slot: 1 }), job(2, { slot: 2 })], waiting: [job(3), job(4, { waiting_on: 'swap 91% > 90%' })] });
    assert.equal(waitReason(s.waiting[0], s), '2/2 heavy busy');
    assert.equal(waitReason(s.waiting[1], s), 'swap 91% > 90%');
    const held = snap({ running: [job(1, { slot: 1 })], waiting: [job(3)], holds: ['load 5.0/core > 4/core'] });
    assert.equal(waitReason(held.waiting[0], held), 'load 5.0/core > 4/core');
    const line = snap({ waiting: [job(3, { class: 'light' }), job(4, { class: 'light' })] });
    assert.equal(waitReason(line.waiting[1], line), '1 ahead in line');
    const lock = 'cargo:/Users/me/.cargo/t';
    const locked = snap({ running: [job(1, { slot: 1, locks: [lock] })], waiting: [job(5, { locks: [lock] })] });
    assert.equal(waitReason(locked.waiting[0], locked), 'cargo:~/.cargo/t held by #1 job 1', 'an older th without waiting_on still names the lock');
});

test('lock holders list who holds a lock and who waits on it', () => {
    const lock = 'cargo:/Users/me/.cargo/shared-target';
    const s = snap({ running: [job(1, { slot: 1, locks: [lock] })], waiting: [job(2, { locks: [lock] }), job(3)] });
    const h = lockHolders(s);
    assert.equal(h.length, 1);
    assert.equal(h[0].holder.ticket, 1);
    assert.deepEqual(
        h[0].waiters.map((w) => w.ticket),
        [2],
    );
    assert.equal(shortLock(lock), 'cargo:~/.cargo/shared-target');
});

test('diff turns two polls into queued / admitted / finished by ticket', () => {
    const a = snap({ running: [job(1, { slot: 1 })], waiting: [job(2)] });
    const b = snap({ running: [job(2, { slot: 1 })], waiting: [job(3)], history: [hist('job 1', 5000, { ticket: 1, exit: 1 })] });
    const ev = diff(a, b);
    assert.deepEqual(
        ev.map((e) => e.kind),
        ['queued', 'admitted', 'finished'],
    );
    const fin = ev.find((e) => e.kind === 'finished');
    assert.equal(fin?.kind === 'finished' ? fin.entry?.exit : undefined, 1);
    assert.deepEqual(diff(null, b), []);
});

test('samples roll over a fixed window and never duplicate', () => {
    const r = snap().readings;
    let s = appendSamples(
        [],
        [
            { t_ms: 0, readings: r },
            { t_ms: 1000, readings: r },
        ],
        5000,
    );
    s = appendSamples(
        s,
        [
            { t_ms: 1000, readings: r },
            { t_ms: 7000, readings: r },
        ],
        5000,
    );
    assert.deepEqual(
        s.map((x) => x.t_ms),
        [7000],
    );
    const t = appendSamples(
        [],
        [
            { t_ms: 0, readings: r },
            { t_ms: 3000, readings: r },
        ],
        5000,
    );
    assert.deepEqual(
        t.map((x) => x.t_ms),
        [0, 3000],
    );
});

test('durations read naturally', () => {
    assert.equal(duration(5_000), '5s');
    assert.equal(duration(125_000), '2m05s');
    assert.equal(duration((3 * 3600 + 7 * 60) * 1000), '3h07m');
});

test('the demo is deterministic, respects slots, and never double-books a lock', () => {
    const a = new DemoQueue(3);
    const b = new DemoQueue(3);
    for (let i = 0; i < 300; i++) {
        const sa = a.step();
        const sb = b.step();
        assert.deepEqual(sa, sb);
        assert.ok(sa.running.filter((j) => j.class === 'heavy').length <= 2);
        assert.ok(sa.running.filter((j) => j.class === 'light').length <= 6);
        const held = sa.running.flatMap((j) => j.locks ?? []);
        assert.equal(new Set(held).size, held.length, 'a lock held twice');
        for (const w of sa.waiting) assert.ok(w.waiting_on, `ticket ${w.ticket} has no reason`);
        for (const j of sa.running) assert.ok(j.slot != null && j.slot >= 1, 'slots are numbered from 1, like the queue');
    }
});

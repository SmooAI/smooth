import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { test } from 'node:test';

import { NightReplay, PERIOD_S, phase } from './ci-queue-demo.ts';
import {
    appendSamples,
    budgetSlices,
    diff,
    duration,
    gaugeFill,
    GATE_X,
    heatOf,
    labelCosts,
    labelP50s,
    lanes,
    lockHolders,
    machineHeat,
    progress,
    reasonHead,
    reasonKind,
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
    assert.equal(waitReason(locked.waiting[0], locked), 'lock cargo:~/.cargo/t held by job 1 (#1)', 'a waiter that has not polled yet still names the lock');
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

test('reasons lead with the part that matters and say what kind of block it is', () => {
    const lock = 'lock cargo:/Users/me/.cargo/shared-target held by clippy (#412) / 2 heavy busy: clippy (pid 1, 40s, repo); tsc (pid 2, 9s, repo)';
    assert.equal(reasonHead(lock), 'lock cargo:/Users/me/.cargo/shared-target held by clippy (#412)');
    assert.equal(reasonKind(lock), 'lock');
    assert.equal(reasonKind('cargo is building outside the queue (/t/debug/.cargo-lock) / 1/2 heavy busy'), 'lock');
    assert.equal(reasonKind('3 ahead in the heavy queue / 2 heavy busy: a (pid 1, 4s, r)'), 'line');
    assert.equal(reasonKind('2 heavy busy: a (pid 1, 4s, r); b (pid 2, 9s, r)'), 'line');
    // a gate reason is 'gate' even though its tail mentions "busy"
    assert.equal(reasonKind('memory pressure critical / swap 91% / load 52.1 on 12 cores / 1/2 heavy busy'), 'gate');
});

test('budget slices come biggest first; committed is what a job may still grow into', () => {
    const s = snap({
        running: [
            job(1, { slot: 1, est: { rss_kb: 1_000, millicores: 2_000, from_runs: 5 }, rss_now_kb: 1_200 }),
            job(2, { slot: 2, est: { rss_kb: 3_000, millicores: 4_000, from_runs: 0 }, rss_now_kb: 1_000 }),
            job(3, { class: 'light', slot: 1 }),
        ],
    });
    const b = budgetSlices(s);
    assert.deepEqual(
        b.map((x) => x.job.ticket),
        [2, 1],
        'a job with no estimate has no slice',
    );
    assert.equal(b[0].committedKb, 2_000);
    assert.equal(b[1].committedKb, 0, 'a job past its estimate commits nothing more');
    assert.equal(b[1].nowKb, 1_200);
    assert.equal(b[0].millicores, 4_000);
});

test("the queue's budget reasons read as budget", () => {
    assert.equal(reasonKind('memory budget: needs 3.4 GB + 2.0 GB committed > 4.1 GB (scale 0.50) / 1/2 heavy busy'), 'budget');
    assert.equal(reasonKind('cpu budget: needs 5.5 + 4.2 cores busy > 9.0 (scale 0.50) / 1/2 heavy busy'), 'budget');
});

test('cost profiles take medians over jobs that carry rusage', () => {
    const c = labelCosts([
        hist('clippy', 10_000, { peak_group_rss_kb: 100, max_single_rss_kb: 40, cpu_ms: 40_000 }),
        hist('clippy', 30_000, { peak_group_rss_kb: 300, max_single_rss_kb: 60, cpu_ms: 60_000 }),
        hist('clippy', 20_000, { peak_group_rss_kb: 200, max_single_rss_kb: 50, cpu_ms: 50_000 }),
        hist('tsc', 5_000, { peak_group_rss_kb: 50, cpu_ms: 5_000 }),
        hist('old', 5_000),
    ]);
    assert.deepEqual(
        c.map((x) => x.label),
        ['clippy', 'tsc'],
    );
    assert.equal(c[0].p50PeakKb, 200);
    assert.equal(c[0].p50RunMs, 20_000);
    assert.equal(c[0].cores, 2.5);
    assert.equal(c[0].p50SingleKb, 50, 'the parallelism gap: 200 across the group, 50 in one process');
    assert.equal(c[1].p50SingleKb, null);
});

test('the night has a calm, a storm and a drain', () => {
    assert.equal(phase(10).name, 'calm');
    assert.equal(phase(150).name, 'storm');
    assert.ok(phase(150).outsideLoad > 900, 'the storm drives load toward 1,000');
    assert.equal(phase(300).name, 'drain');
    assert.equal(phase(PERIOD_S + 10).name, 'calm', 'it loops');
});

test('the night replay is deterministic, respects slots and locks, and holds in the storm', () => {
    const a = new NightReplay(3);
    const b = new NightReplay(3);
    let peakLoad = 0;
    let held = false;
    let minScale = 1;
    for (let i = 0; i < PERIOD_S; i++) {
        const sa = a.step();
        const sb = b.step();
        assert.deepEqual(sa, sb);
        assert.equal(sa.schema, 2);
        assert.ok(sa.running.filter((j) => j.class === 'heavy').length <= 2);
        assert.ok(sa.running.filter((j) => j.class === 'light').length <= 6);
        const heldLocks = sa.running.flatMap((j) => j.locks ?? []);
        assert.equal(new Set(heldLocks).size, heldLocks.length, 'a lock held twice');
        for (const w of sa.waiting) assert.ok(w.waiting_on, `ticket ${w.ticket} has no reason`);
        for (const j of sa.running) assert.ok(j.slot != null && j.slot >= 1, 'slots are numbered from 1, like the queue');
        peakLoad = Math.max(peakLoad, sa.readings.load1 ?? 0);
        held ||= sa.holds.length > 0;
        minScale = Math.min(minScale, sa.budget?.scale ?? 1);
        for (const j of sa.running) assert.ok(j.est && j.est.millicores > 0, 'running jobs carry their estimate');
        assert.ok((sa.budget?.mem_committed_kb ?? -1) >= 0);
    }
    assert.ok(peakLoad > 700, `load peaked at ${peakLoad}`);
    assert.ok(held, 'the gate held during the storm');
    assert.ok(minScale < 0.6, 'the budget backed off');
});

// A real `th ci-queue status --json` from the queue's schema-2 build (#676;
// paths sanitised): one heavy job holding `--lock docker`, one waiter blocked
// on it, finished jobs with rusage. The page must read it as shipped.
const REAL = JSON.parse(readFileSync(new URL('./ci-queue.sample.json', import.meta.url), 'utf8')) as Snapshot;

test('the real schema-2 snapshot reads as shipped', () => {
    assert.equal(REAL.schema, 2);
    assert.equal(REAL.config.budget?.aimd_max, 2);
    const [running] = REAL.running;
    assert.equal(running.slot, 1, 'slots are 1-based');
    assert.equal(lanes(REAL, 'heavy')[0]?.ticket, running.ticket);
    // the waiter's reason is the queue's own words, led by the lock
    const [waiter] = REAL.waiting;
    assert.equal(reasonKind(waitReason(waiter, REAL)), 'lock');
    assert.equal(reasonHead(waitReason(waiter, REAL)), `lock docker held by ${running.label} (#${running.ticket})`);
    assert.equal(waiter.blocked_by_ticket, running.ticket);
    // budget: one slice, committed = est − now
    const [slice] = budgetSlices(REAL);
    assert.equal(slice.estKb, running.est?.rss_kb);
    assert.equal(slice.committedKb, (running.est?.rss_kb ?? 0) - (running.rss_now_kb ?? 0));
    assert.equal(REAL.budget?.mem_committed_kb, slice.committedKb, "matches the queue's own sum");
    // cost profiles from rusage history; optional fields may be absent
    assert.ok(labelCosts(REAL.history).length > 0);
    assert.ok(signals(REAL.readings, REAL.config.gate).every((x) => x.value !== 'unknown'));
});

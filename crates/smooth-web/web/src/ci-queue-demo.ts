//! `?demo`: a replay of the night the queue was built for (SMOODEV-3371).
//!
//! 2026-09-26: about 35 agent sessions on one 12-core, 24 GB Mac committed at
//! once. Load average reached 1,022 and swap filled (22 of 23.5 GB). This
//! plays that night back against the queue: a calm start, the storm (agents
//! pile in, work outside the queue drives load toward 1,000, memory runs
//! out, the gate holds heavy jobs), then the drain. It loops every
//! `PERIOD_S` seconds of simulated time.
//!
//! It is seeded and deterministic, so the tests and the screenshots see the
//! same night every time. It speaks the real snapshot shape, including the
//! queue's own reason strings and the schema-2 fields (budget, estimates,
//! rusage) that ship after SMOODEV-3355, so the page can be shown when the
//! machine is idle, or before the queue grows those fields.

import type { Budget, HistoryEntry, JobClass, JobInfo, Readings, Sample, Snapshot } from './ci-queue.ts';

const GB_BYTES = 1_073_741_824;
const GB_KB = 1_048_576;
const CORES = 12;
const RAM_GB = 24;
const SWAP_GB = 23.5;
const CARGO_LOCK = 'cargo:/Users/dev/.cargo/shared-target';

/** Seconds in one loop of the night. */
export const PERIOD_S = 480;

interface Kind {
    label: string;
    cls: JobClass;
    cargo?: boolean;
    /** Typical run time. */
    ms: number;
    /** Typical peak RSS of the whole process group, GB. */
    rssGb: number;
    /** Cores it keeps busy while it runs. */
    cores: number;
}

const KINDS: Kind[] = [
    { label: 'cargo clippy --workspace', cls: 'heavy', cargo: true, ms: 95_000, rssGb: 3.4, cores: 5.5 },
    { label: 'cargo test -p smooth-cli', cls: 'heavy', cargo: true, ms: 140_000, rssGb: 4.6, cores: 6.5 },
    { label: 'pnpm turbo typecheck', cls: 'heavy', ms: 70_000, rssGb: 2.9, cores: 4.2 },
    { label: 'tsgo --noEmit', cls: 'heavy', ms: 38_000, rssGb: 1.7, cores: 2.1 },
    { label: 'vitest run packages/backend', cls: 'heavy', ms: 55_000, rssGb: 1.3, cores: 3.0 },
    { label: 'oxfmt --check', cls: 'light', ms: 9_000, rssGb: 0.15, cores: 0.9 },
    { label: 'cargo fmt --check', cls: 'light', ms: 7_000, rssGb: 0.1, cores: 0.8 },
    { label: 'oxlint .', cls: 'light', ms: 12_000, rssGb: 0.3, cores: 1.4 },
    { label: 'lint-staged', cls: 'light', ms: 16_000, rssGb: 0.4, cores: 1.0 },
];

/** 35 agent worktrees, the sessions that were committing that night. */
const WORKTREES = Array.from({ length: 35 }, (_, i) => {
    const tickets = [3342, 3323, 3207, 3352, 3355, 3349, 3301, 3288, 3310, 3336, 3274, 3319];
    const topic = [
        'models',
        'hitl',
        'ask-smooth',
        'o11y',
        'ci-queue',
        'crm-emails',
        'llm-caps',
        'dashboard',
        'span-pii',
        'heypage-photos',
        'trial-abuse',
        'auth-pages',
    ];
    const repo = i % 3 === 0 ? 'smooth' : 'smooai';
    return `/Users/dev/${repo}-SMOODEV-${tickets[i % tickets.length]}-${topic[i % topic.length]}${i >= 12 ? `-${Math.floor(i / 12) + 1}` : ''}`;
});

/** The night's shape at `t` seconds into a loop. */
export function phase(t: number): { name: 'calm' | 'storm' | 'drain'; arrivals: number; outsideLoad: number; memPressure: number } {
    const x = ((t % PERIOD_S) + PERIOD_S) % PERIOD_S;
    const ramp = (a: number, b: number) => Math.min(1, Math.max(0, (x - a) / (b - a)));
    if (x < 60) return { name: 'calm', arrivals: 0.12, outsideLoad: 24, memPressure: 0.2 };
    if (x < 240) {
        // Everyone commits at once; work outside the queue climbs toward 1,000.
        const up = ramp(60, 150);
        const down = ramp(200, 240);
        return { name: 'storm', arrivals: 0.55 * (1 - down) + 0.1, outsideLoad: 24 + 990 * up * (1 - down), memPressure: 0.2 + 0.8 * up * (1 - down * 0.7) };
    }
    return { name: 'drain', arrivals: 0.08, outsideLoad: 30, memPressure: 0.25 };
}

export class NightReplay {
    private seed: number;
    private ticket = 400;
    private start: number;
    private now: number;
    private running: Array<JobInfo & { due: number; kind: Kind }> = [];
    private waiting: Array<JobInfo & { kind: Kind }> = [];
    private history: HistoryEntry[] = [];
    private load = 24;
    private memAvailPct = 38;
    private swapGb = 9;
    private scale = 1;
    private lastBackoff = -Infinity;

    constructor(seed = 11, startMs = Date.UTC(2026, 8, 27, 3, 0, 0)) {
        this.seed = seed;
        this.start = startMs;
        this.now = startMs;
        // An evening's worth of finished jobs, so p50s and cost profiles exist.
        for (let i = 0; i < 90; i++) {
            const k = KINDS[i % KINDS.length];
            const run = Math.round(k.ms * (0.7 + this.rand() * 0.6));
            const peak = k.rssGb * GB_KB * (0.75 + this.rand() * 0.5);
            this.history.push({
                label: k.label,
                class: k.cls,
                cwd: this.worktree(),
                ticket: this.ticket++,
                queued_at_ms: this.now - (90 - i) * 40_000,
                wait_ms: Math.round(this.rand() * 20_000),
                run_ms: run,
                outcome: 'exit',
                exit: this.rand() < 0.1 ? 1 : 0,
                peak_group_rss_kb: Math.round(peak),
                max_single_rss_kb: Math.round(peak * (0.45 + this.rand() * 0.3)),
                cpu_ms: Math.round(run * k.cores * (0.8 + this.rand() * 0.4)),
            });
        }
    }

    /** Seconds into the night. */
    get t(): number {
        return (this.now - this.start) / 1000;
    }

    private rand(): number {
        // mulberry32
        let t = (this.seed += 0x6d2b79f5);
        t = Math.imul(t ^ (t >>> 15), t | 1);
        t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
        return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
    }

    private worktree(): string {
        return WORKTREES[Math.floor(this.rand() * WORKTREES.length)];
    }

    private arrive(): void {
        // Commits are mostly heavy checks; the light ones ride along.
        const k = this.rand() < 0.55 ? KINDS[Math.floor(this.rand() * 5)] : KINDS[5 + Math.floor(this.rand() * 4)];
        const ticket = this.ticket++;
        this.waiting.push({
            class: k.cls,
            ticket,
            label: k.label,
            pid: 40_000 + ticket,
            cwd: this.worktree(),
            queued_at_ms: this.now,
            locks: k.cargo ? [CARGO_LOCK] : undefined,
            kind: k,
        });
    }

    private readings(): Readings {
        return {
            mem_total_bytes: RAM_GB * GB_BYTES,
            mem_available_bytes: Math.round((RAM_GB * GB_BYTES * this.memAvailPct) / 100),
            memory_pressure_level: this.memAvailPct < 4 ? 4 : this.memAvailPct < 9 ? 2 : 1,
            swap_total_bytes: Math.round(SWAP_GB * GB_BYTES),
            swap_used_bytes: Math.round(this.swapGb * GB_BYTES),
            load1: this.load,
            cores: CORES,
            disks: [
                { path: '/Users/dev/.cargo/shared-target', free_bytes: 96 * GB_BYTES },
                { path: '/Users/dev', free_bytes: 96 * GB_BYTES },
            ],
        };
    }

    /** The gate's verdict, in the queue's words (reading only, like `pressure::holds`). */
    private holds(): string[] {
        const r = this.readings();
        const out: string[] = [];
        if (this.memAvailPct < 5) out.push(`memory ${Math.round(this.memAvailPct)}% available`);
        const level = r.memory_pressure_level ?? 1;
        if (level > 1) out.push(`memory pressure ${level >= 4 ? 'critical' : 'warn'}`);
        const swapPct = (this.swapGb * 100) / SWAP_GB;
        if (swapPct > 90 && (level > 1 || this.memAvailPct < 10)) out.push(`swap ${Math.round(swapPct)}%`);
        if (this.load / CORES > 4) out.push(`load ${this.load.toFixed(1)} on ${CORES} cores`);
        return out;
    }

    private busySummary(cls: JobClass): string {
        const busy = this.running.filter((r) => r.class === cls);
        const n = cls === 'heavy' ? 2 : 6;
        if (busy.length < n) return `${busy.length}/${n} ${cls} busy`;
        const parts = busy.map(
            (b) => `${b.label} (pid ${b.pid}, ${Math.round((this.now - (b.admitted_at_ms ?? this.now)) / 1000)}s, ${b.cwd.split('/').pop()})`,
        );
        return `${n} ${cls} busy: ${parts.join('; ')}`;
    }

    private budgetBase(): { memKb: number; cores: number } {
        return { memKb: 16 * GB_KB, cores: 10 };
    }

    private admit(): void {
        const holds = this.holds();
        for (const cls of ['heavy', 'light'] as const) {
            const slots = cls === 'heavy' ? 2 : 6;
            const line = this.waiting.filter((w) => w.class === cls).sort((a, b) => a.ticket - b.ticket);
            let ahead = 0;
            for (const w of line) {
                const busy = this.running.filter((r) => r.class === cls);
                const lock = (w.locks ?? []).find((l) => this.running.some((r) => (r.locks ?? []).includes(l)));
                const summary = this.busySummary(cls);
                let reason: string | null = null;
                let by: number | undefined;
                if (lock) {
                    const holder = this.running.find((r) => (r.locks ?? []).includes(lock));
                    reason = `lock ${lock} held by ${holder?.label} (#${holder?.ticket}) / ${summary}`;
                    by = holder?.ticket;
                } else if (ahead > 0) {
                    reason = `${ahead} ahead in the ${cls} queue / ${summary}`;
                } else if (busy.length >= slots) {
                    reason = summary;
                } else if (cls === 'heavy' && holds.length > 0 && busy.length > 0) {
                    reason = `${holds.join(' / ')} / ${summary}`;
                } else if (cls === 'heavy' && this.running.some((r) => r.class === 'heavy')) {
                    // Capacity-aware admission: a heavy job must fit the
                    // budget, unless no other heavy job is running.
                    const b = this.budget();
                    const needKb = w.kind.rssGb * GB_KB;
                    const freeKb = b.mem_kb - b.mem_used_kb;
                    const freeCores = b.cores - b.cores_used;
                    if (needKb > freeKb)
                        reason = `budget: needs ${(needKb / GB_KB).toFixed(1)} GB, ${(Math.max(0, freeKb) / GB_KB).toFixed(1)} GB free / ${summary}`;
                    else if (w.kind.cores > freeCores + 0.5)
                        reason = `budget: needs ${w.kind.cores.toFixed(1)} cores, ${Math.max(0, freeCores).toFixed(1)} free / ${summary}`;
                }
                if (reason) {
                    if (w.waiting_on?.split(' / ')[0] !== reason.split(' / ')[0]) w.waiting_on_since_ms = this.now;
                    w.waiting_on = reason;
                    w.blocked_by_ticket = by;
                    // A waiter blocked only on a lock does not hold up the line behind it.
                    if (!lock) ahead++;
                    continue;
                }
                const used = new Set(busy.map((b) => b.slot));
                let slot = 1;
                while (used.has(slot)) slot++;
                this.waiting = this.waiting.filter((x) => x.ticket !== w.ticket);
                const k = w.kind;
                this.running.push({
                    ...w,
                    admitted_at_ms: this.now,
                    slot,
                    child_pid: w.pid + 1,
                    waiting_on: undefined,
                    waiting_on_since_ms: undefined,
                    blocked_by_ticket: undefined,
                    est_rss_kb: Math.round(k.rssGb * GB_KB),
                    est_cores: k.cores,
                    rss_kb: Math.round(k.rssGb * GB_KB * 0.3),
                    cores_now: k.cores * 0.5,
                    due: this.now + Math.round(k.ms * (0.6 + this.rand() * 0.9) * (1 + Math.max(0, this.load / CORES - 4) * 0.05)),
                });
            }
        }
    }

    /** Advance `ms` of simulated time; returns the new snapshot. */
    step(ms = 1000): Snapshot {
        this.now += ms;
        const p = phase(this.t);
        // Finish what is due.
        for (const j of [...this.running]) {
            if (j.due > this.now) continue;
            this.running = this.running.filter((r) => r.ticket !== j.ticket);
            const run = this.now - (j.admitted_at_ms ?? this.now);
            const peak = j.kind.rssGb * GB_KB * (0.75 + this.rand() * 0.5);
            this.history.push({
                label: j.label,
                class: j.class,
                cwd: j.cwd,
                ticket: j.ticket,
                queued_at_ms: j.queued_at_ms,
                wait_ms: (j.admitted_at_ms ?? j.queued_at_ms) - j.queued_at_ms,
                run_ms: run,
                outcome: 'exit',
                exit: this.rand() < 0.12 ? 1 : 0,
                peak_group_rss_kb: Math.round(peak),
                max_single_rss_kb: Math.round(peak * (0.45 + this.rand() * 0.3)),
                cpu_ms: Math.round(run * j.kind.cores * (0.8 + this.rand() * 0.4)),
            });
        }
        if (this.history.length > 300) this.history.splice(0, this.history.length - 300);
        // Agents commit.
        if (this.rand() < p.arrivals * (ms / 1000) && this.waiting.length < 28) this.arrive();
        // Running jobs ramp up to their working set.
        for (const r of this.running) {
            r.rss_kb = Math.round((r.rss_kb ?? 0) + ((r.est_rss_kb ?? 0) * (0.8 + this.rand() * 0.5) - (r.rss_kb ?? 0)) * 0.12);
            r.cores_now = (r.cores_now ?? 0) + ((r.est_cores ?? 0) * (0.7 + this.rand() * 0.5) - (r.cores_now ?? 0)) * 0.2;
        }
        // The machine: queued work plus everything outside the queue.
        const inQueue = this.running.reduce((a, r) => a + (r.cores_now ?? 0), 0);
        const targetLoad = p.outsideLoad + inQueue * 1.4 + this.rand() * 6;
        this.load += (targetLoad - this.load) * 0.1;
        const usedGb = this.running.reduce((a, r) => a + (r.rss_kb ?? 0) / GB_KB, 0);
        const targetAvail = Math.max(1.5, 42 - p.memPressure * 40 - usedGb * 1.2);
        this.memAvailPct += (targetAvail - this.memAvailPct) * 0.08;
        this.swapGb = Math.min(22.4, Math.max(8, this.swapGb + (this.memAvailPct < 10 ? 0.12 : -0.02) * (ms / 1000)));
        // AIMD: grow the budget while calm, halve it on a spike.
        const spike = this.holds().length > 0;
        if (spike && this.now - this.lastBackoff > 15_000) {
            this.scale = Math.max(0.35, this.scale * 0.5);
            this.lastBackoff = this.now;
        } else if (!spike && this.now % 5000 < ms) {
            this.scale = Math.min(1.25, this.scale + 0.05);
        }
        this.admit();
        return this.snapshot();
    }

    budget(): Budget {
        const base = this.budgetBase();
        return {
            mem_kb: Math.round(base.memKb * this.scale),
            mem_used_kb: this.running.reduce((a, r) => a + (r.est_rss_kb ?? 0), 0),
            cores: +(base.cores * this.scale).toFixed(2),
            cores_used: +this.running.reduce((a, r) => a + (r.est_cores ?? 0), 0).toFixed(2),
            scale: +this.scale.toFixed(3),
            scale_min: 0.35,
            scale_max: 1.25,
        };
    }

    snapshot(): Snapshot {
        const strip = (j: JobInfo & { kind: Kind; due?: number }): JobInfo => {
            const { kind: _k, due: _d, ...rest } = j;
            return rest;
        };
        return {
            schema: 2,
            dir: '/Users/dev/.smooth/ci-queue',
            config: {
                slots: { heavy: 2, light: 6 },
                gate: { min_available_memory_pct: 5, max_memory_pressure_level: 1, max_swap_used_pct: 90, max_load_per_core: 4, min_free_disk_gb: 20 },
            },
            now_ms: this.now,
            running: this.running.map(strip),
            waiting: this.waiting.map(strip).sort((a, b) => a.ticket - b.ticket),
            readings: this.readings(),
            holds: this.holds(),
            history: [...this.history],
            budget: this.budget(),
        };
    }

    sample(): Sample {
        return { t_ms: this.now, readings: this.readings(), budget: this.budget() };
    }
}

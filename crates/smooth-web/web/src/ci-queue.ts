//! The ci-queue panel's data: the `th ci-queue status --json` shape, what the
//! page derives from it, and a simulator for `?demo` (SMOODEV-3371).
//!
//! Everything here is pure so it is tested under `node --test`. The rules that
//! matter:
//! - **The gate's own `holds` are the truth.** The heat a signal shows is only
//!   an estimate of how close it is to its threshold; whether new heavy jobs
//!   are held is what `holds` says, never what we recompute.
//! - **Every gauge draws "worse" to the right, with its threshold at the same
//!   x** (`GATE_X`), so the thresholds line up into one gate line down the
//!   panel and a reading past it is visibly past it.
//! - Rust's `th ci-queue top` makes the same calls (`ci_queue/top.rs`); keep
//!   the two in step.

export type JobClass = 'heavy' | 'light';

export interface JobInfo {
    class: JobClass;
    ticket: number;
    label: string;
    pid: number;
    child_pid?: number;
    cwd: string;
    queued_at_ms: number;
    admitted_at_ms?: number;
    slot?: number;
    locks?: string[];
    /** Why this waiter has not been admitted, in the queue's own words. */
    waiting_on?: string;
    waiting_on_since_ms?: number;
    /** When `waiting_on` is a named lock: the ticket holding it. */
    blocked_by_ticket?: number;
}

export interface HistoryEntry {
    label: string;
    class: JobClass;
    cwd: string;
    ticket: number;
    queued_at_ms: number;
    wait_ms: number;
    run_ms: number;
    /** `exit`, `signal`, `timeout`, `spawn-failed`, or `wait-timeout`. */
    outcome: string;
    exit: number;
}

export interface Disk {
    path: string;
    free_bytes?: number | null;
}

export interface Readings {
    mem_total_bytes?: number | null;
    mem_available_bytes?: number | null;
    memory_pressure_level?: number | null;
    swap_total_bytes?: number | null;
    swap_used_bytes?: number | null;
    load1?: number | null;
    cores: number;
    disks: Disk[];
}

export interface Gate {
    min_available_memory_pct: number;
    max_memory_pressure_level: number;
    max_swap_used_pct: number;
    max_load_per_core: number;
    min_free_disk_gb: number;
}

export interface Snapshot {
    schema?: number;
    dir: string;
    config: { slots: { heavy: number; light: number }; gate: Gate };
    now_ms: number;
    running: JobInfo[];
    waiting: JobInfo[];
    readings: Readings;
    holds: string[];
    history: HistoryEntry[];
}

/** One pressure sample the daemon kept (it samples while the panel is open). */
export interface Sample {
    t_ms: number;
    readings: Readings;
}

/** `GET /api/ci-queue/status`. */
export interface StatusResponse {
    snapshot: Snapshot | null;
    samples: Sample[];
    /** Set when the daemon could not read the queue — shown, never hidden. */
    error?: string;
}

/** The schema this client understands. A newer snapshot is shown with a warning. */
export const SCHEMA = 1;

/** Where every gauge's threshold sits, as a fraction of the gauge's width. */
export const GATE_X = 0.8;

const GB = 1_073_741_824;

// ── Heat ─────────────────────────────────────────────────────────────────────

/** The Aurora spectrum, cool → hot. Index = heat level. */
export const HEAT = ['#00a6a6', '#12b39a', '#7cb352', '#f49f0a', '#ff8a3d', '#ff6b6c'] as const;
export type Heat = 0 | 1 | 2 | 3 | 4 | 5;

/** How close a signal is to its threshold (1 = at it) → a heat level. */
export function heatOf(ratio: number): Heat {
    if (!Number.isFinite(ratio) || ratio < 0.5) return 0;
    if (ratio < 0.7) return 1;
    if (ratio < 0.85) return 2;
    if (ratio < 1) return 3;
    if (ratio < 1.15) return 4;
    return 5;
}

// ── Signals ──────────────────────────────────────────────────────────────────

export type SignalKey = 'memory' | 'pressure' | 'swap' | 'load' | 'disk';

export interface Signal {
    key: SignalKey;
    name: string;
    /** The reading, in words ("88% used"), or "unknown". */
    value: string;
    /** When it holds, in words ("holds above 95%"). */
    limit: string;
    /** reading ÷ threshold, "worse" direction. 1 = at the threshold. null = unknown. */
    ratio: number | null;
    /** The gate has this signal turned off (threshold 0). */
    off: boolean;
    /** A signal that only counts under another condition (swap). */
    note?: string;
}

export function memAvailablePct(r: Readings): number | null {
    return r.mem_available_bytes != null && r.mem_total_bytes ? (r.mem_available_bytes * 100) / r.mem_total_bytes : null;
}

export function swapUsedPct(r: Readings): number | null {
    if (r.swap_used_bytes == null || r.swap_total_bytes == null) return null;
    return r.swap_total_bytes === 0 ? 0 : (r.swap_used_bytes * 100) / r.swap_total_bytes;
}

export function loadPerCore(r: Readings): number | null {
    return r.load1 != null && r.cores > 0 ? r.load1 / r.cores : null;
}

/** The tightest watched volume, in GB free. */
export function minFreeDiskGb(r: Readings): { gb: number; path: string } | null {
    let best: { gb: number; path: string } | null = null;
    for (const d of r.disks) {
        if (d.free_bytes == null) continue;
        const gb = d.free_bytes / GB;
        if (!best || gb < best.gb) best = { gb, path: d.path };
    }
    return best;
}

export function pressureName(level: number): string {
    if (level >= 4) return 'critical';
    if (level >= 2) return 'warn';
    return 'normal';
}

/** The kernel's own memory-pressure level (macOS) as a ratio: at or under the
 * limit is comfortably cool, over it is past the gate. */
function pressureRatio(level: number, max: number): number {
    if (level <= max) return 0.35 * (level / Math.max(1, max));
    return level >= 4 ? 1.3 : 1.05;
}

/** Each gate signal against its threshold, in the gate's order. */
export function signals(r: Readings, g: Gate): Signal[] {
    const out: Signal[] = [];
    const avail = memAvailablePct(r);
    const memLimit = 100 - g.min_available_memory_pct;
    out.push({
        key: 'memory',
        name: 'Memory',
        value: avail == null ? 'unknown' : `${Math.round(100 - avail)}% used`,
        limit: `holds above ${Math.round(memLimit)}% used`,
        ratio: avail == null || memLimit <= 0 ? null : (100 - avail) / memLimit,
        off: g.min_available_memory_pct <= 0,
    });
    if (r.memory_pressure_level != null) {
        out.push({
            key: 'pressure',
            name: 'Memory pressure',
            value: pressureName(r.memory_pressure_level),
            limit: `holds above ${pressureName(g.max_memory_pressure_level)}`,
            ratio: pressureRatio(r.memory_pressure_level, g.max_memory_pressure_level),
            off: g.max_memory_pressure_level <= 0,
        });
    }
    const swap = swapUsedPct(r);
    out.push({
        key: 'swap',
        name: 'Swap',
        value: swap == null ? 'unknown' : `${Math.round(swap)}% used`,
        limit: `holds above ${Math.round(g.max_swap_used_pct)}%`,
        ratio: swap == null || g.max_swap_used_pct <= 0 ? null : swap / g.max_swap_used_pct,
        off: g.max_swap_used_pct <= 0,
        note: 'only while memory is tight',
    });
    const lpc = loadPerCore(r);
    out.push({
        key: 'load',
        name: 'Load',
        value: lpc == null || r.load1 == null ? 'unknown' : `${r.load1.toFixed(0)} · ${lpc.toFixed(1)}/core`,
        limit: `holds above ${g.max_load_per_core}/core`,
        ratio: lpc == null || g.max_load_per_core <= 0 ? null : lpc / g.max_load_per_core,
        off: g.max_load_per_core <= 0,
    });
    const disk = minFreeDiskGb(r);
    out.push({
        key: 'disk',
        name: 'Disk',
        value: disk == null ? 'unknown' : `${Math.round(disk.gb)} GB free`,
        limit: `holds below ${g.min_free_disk_gb} GB`,
        ratio: disk == null || g.min_free_disk_gb <= 0 ? null : g.min_free_disk_gb / Math.max(0.1, disk.gb),
        off: g.min_free_disk_gb <= 0,
    });
    return out;
}

/** Memory is tight enough for swap to count (mirrors the Rust gate). */
function memoryTight(r: Readings, g: Gate): boolean {
    const avail = memAvailablePct(r);
    const level = r.memory_pressure_level ?? 1;
    return level > 1 || (g.min_available_memory_pct > 0 && avail != null && avail < 2 * g.min_available_memory_pct);
}

/** The machine's overall heat: the hottest live signal, pinned to the gate's
 * verdict — never hotter than gold while nothing is held, never cooler than
 * orange while something is. */
export function machineHeat(snap: Snapshot): Heat {
    const tight = memoryTight(snap.readings, snap.config.gate);
    let worst = 0;
    for (const s of signals(snap.readings, snap.config.gate)) {
        if (s.off || s.ratio == null) continue;
        if (s.key === 'swap' && !tight) continue;
        worst = Math.max(worst, s.ratio);
    }
    const h = heatOf(worst);
    if (snap.holds.length > 0) return Math.max(4, h) as Heat;
    return Math.min(3, h) as Heat;
}

/** Where a ratio lands on a gauge whose threshold sits at `GATE_X`. */
export function gaugeFill(ratio: number | null): number {
    if (ratio == null || !Number.isFinite(ratio)) return 0;
    return Math.max(0, Math.min(1, ratio * GATE_X));
}

/** One signal's ratio over a sample stream, for its sparkline. */
export function signalSeries(samples: Sample[], g: Gate, key: SignalKey): Array<number | null> {
    return samples.map((s) => signals(s.readings, g).find((x) => x.key === key)?.ratio ?? null);
}

// ── Jobs ─────────────────────────────────────────────────────────────────────

/** Median run time per label, from finished jobs that actually ran. */
export function labelP50s(history: HistoryEntry[]): Map<string, number> {
    const runs = new Map<string, number[]>();
    for (const h of history) {
        if (h.outcome === 'wait-timeout' || h.outcome === 'spawn-failed' || h.run_ms <= 0) continue;
        const list = runs.get(h.label) ?? [];
        list.push(h.run_ms);
        runs.set(h.label, list);
    }
    const out = new Map<string, number>();
    for (const [label, list] of runs) {
        list.sort((a, b) => a - b);
        const mid = Math.floor(list.length / 2);
        out.set(label, list.length % 2 ? list[mid] : (list[mid - 1] + list[mid]) / 2);
    }
    return out;
}

/** A running job's elapsed time against its label's p50. `null` when the
 * label has never finished here (the lane shows it indeterminate). */
export function progress(job: JobInfo, nowMs: number, p50s: Map<string, number>): { elapsedMs: number; frac: number | null; over: boolean } {
    const elapsedMs = Math.max(0, nowMs - (job.admitted_at_ms ?? job.queued_at_ms));
    const p50 = p50s.get(job.label);
    if (!p50) return { elapsedMs, frac: null, over: false };
    return { elapsedMs, frac: Math.min(1, elapsedMs / p50), over: elapsedMs > p50 };
}

/** One lane per slot, `null` where the slot is free. The queue numbers its
 * slots from 1 (`heavy-1.slot`), so slot n draws in lane n-1. */
export function lanes(snap: Snapshot, cls: JobClass): Array<JobInfo | null> {
    const n = snap.config.slots[cls];
    const out: Array<JobInfo | null> = Array.from({ length: n }, () => null);
    const extra: JobInfo[] = [];
    for (const j of snap.running) {
        if (j.class !== cls) continue;
        if (j.slot != null && j.slot >= 1 && j.slot <= n && out[j.slot - 1] == null) out[j.slot - 1] = j;
        else extra.push(j);
    }
    // A slot past the configured count (the config shrank while it ran) still shows.
    return [...out, ...extra];
}

/** The waiter's reason, falling back to what the queue implies when an older
 * `th` does not report one. */
export function waitReason(j: JobInfo, snap: Snapshot): string {
    if (j.waiting_on) return j.waiting_on;
    for (const lock of j.locks ?? []) {
        const holder = snap.running.find((r) => (r.locks ?? []).includes(lock));
        if (holder) return `${shortLock(lock)} held by #${holder.ticket} ${holder.label}`;
    }
    const busy = snap.running.filter((r) => r.class === j.class).length;
    const slots = snap.config.slots[j.class];
    if (busy >= slots) return `${busy}/${slots} ${j.class} busy`;
    if (j.class === 'heavy' && snap.holds.length > 0) return snap.holds[0];
    const ahead = snap.waiting.filter((w) => w.class === j.class && w.ticket < j.ticket).length;
    return ahead > 0 ? `${ahead} ahead in line` : 'next in line';
}

/** Which named locks are held, and by whom. */
export function lockHolders(snap: Snapshot): Array<{ lock: string; holder: JobInfo; waiters: JobInfo[] }> {
    const out: Array<{ lock: string; holder: JobInfo; waiters: JobInfo[] }> = [];
    for (const j of snap.running) {
        for (const lock of j.locks ?? []) {
            out.push({ lock, holder: j, waiters: snap.waiting.filter((w) => (w.locks ?? []).includes(lock)) });
        }
    }
    return out;
}

/** `cargo:/Users/x/.cargo/shared-target` → `cargo:~/.cargo/shared-target`. */
export function shortLock(lock: string): string {
    return lock.replace(/\/(?:Users|home)\/[^/]+/, '~');
}

/** The last path segment of a cwd, which is usually the worktree name. */
export function worktreeOf(cwd: string): string {
    const parts = cwd.split('/').filter(Boolean);
    return parts[parts.length - 1] ?? cwd;
}

export function duration(ms: number): string {
    const s = Math.round(ms / 1000);
    if (s >= 3600) return `${Math.floor(s / 3600)}h${String(Math.floor((s % 3600) / 60)).padStart(2, '0')}m`;
    if (s >= 60) return `${Math.floor(s / 60)}m${String(s % 60).padStart(2, '0')}s`;
    return `${s}s`;
}

// ── Transitions between snapshots ────────────────────────────────────────────

export type QueueEvent =
    | { kind: 'queued'; job: JobInfo }
    | { kind: 'admitted'; job: JobInfo }
    | { kind: 'finished'; ticket: number; entry: HistoryEntry | null };

/** What changed between two polls, by ticket. The queue has no event log, so
 * this is how the panel knows a job moved. */
export function diff(prev: Snapshot | null, next: Snapshot): QueueEvent[] {
    if (!prev) return [];
    const out: QueueEvent[] = [];
    const wasWaiting = new Set(prev.waiting.map((j) => j.ticket));
    const wasRunning = new Set(prev.running.map((j) => j.ticket));
    const isRunning = new Set(next.running.map((j) => j.ticket));
    for (const j of next.waiting) if (!wasWaiting.has(j.ticket) && !wasRunning.has(j.ticket)) out.push({ kind: 'queued', job: j });
    for (const j of next.running) if (!wasRunning.has(j.ticket)) out.push({ kind: 'admitted', job: j });
    for (const t of wasRunning) {
        if (!isRunning.has(t)) out.push({ kind: 'finished', ticket: t, entry: next.history.find((h) => h.ticket === t) ?? null });
    }
    return out;
}

/** Keep a rolling window of samples, oldest first. */
export function appendSamples(prev: Sample[], add: Sample[], windowMs: number): Sample[] {
    const merged = [...prev];
    const last = merged.length ? merged[merged.length - 1].t_ms : -Infinity;
    for (const s of add) if (s.t_ms > last) merged.push(s);
    const cutoff = (merged.length ? merged[merged.length - 1].t_ms : 0) - windowMs;
    return merged.filter((s) => s.t_ms >= cutoff);
}

// ── Demo feed ────────────────────────────────────────────────────────────────

const DEMO_JOBS: Array<{ label: string; cls: JobClass; cargo?: boolean; ms: number; cwd: string }> = [
    { label: 'cargo clippy --workspace', cls: 'heavy', cargo: true, ms: 95_000, cwd: '/Users/dev/smooth-SMOODEV-3342-models' },
    { label: 'pnpm turbo typecheck', cls: 'heavy', ms: 70_000, cwd: '/Users/dev/smooai-SMOODEV-3323-hitl' },
    { label: 'cargo test -p smooth-cli', cls: 'heavy', cargo: true, ms: 140_000, cwd: '/Users/dev/smooth-SMOODEV-3355-ci-queue' },
    { label: 'tsgo --noEmit', cls: 'heavy', ms: 38_000, cwd: '/Users/dev/smooai-SMOODEV-3207-ask-smooth' },
    { label: 'vitest run packages/backend', cls: 'heavy', ms: 55_000, cwd: '/Users/dev/smooai-SMOODEV-3352-o11y' },
    { label: 'oxfmt --check', cls: 'light', ms: 16_000, cwd: '/Users/dev/smooai-SMOODEV-3323-hitl' },
    { label: 'cargo fmt --check', cls: 'light', ms: 11_000, cwd: '/Users/dev/smooth-th-1efb59-flow-mcp' },
    { label: 'oxlint .', cls: 'light', ms: 21_000, cwd: '/Users/dev/smooai-SMOODEV-3207-ask-smooth' },
    { label: 'lint-staged', cls: 'light', ms: 26_000, cwd: '/Users/dev/smooai-SMOODEV-3352-o11y' },
];

const CARGO_LOCK = 'cargo:/Users/dev/.cargo/shared-target';

/** A deterministic, seeded simulation of a busy machine: heavy jobs pile up,
 * pressure rises with them, cargo jobs fight over the shared target. Used by
 * `?demo`, the screenshots, and the tests. */
export class DemoQueue {
    private seed: number;
    private ticket = 400;
    private now: number;
    private running: JobInfo[] = [];
    private waiting: JobInfo[] = [];
    private history: HistoryEntry[] = [];
    private due = new Map<number, number>();
    private load = 38;
    private swapUsed = 17.5;
    private memAvail = 14;

    constructor(seed = 7, startMs = Date.UTC(2026, 8, 26, 23, 0, 0)) {
        this.seed = seed;
        this.now = startMs;
        // Seed history so p50s exist from the first frame.
        for (let i = 0; i < 40; i++) {
            const d = DEMO_JOBS[i % DEMO_JOBS.length];
            const run = Math.round(d.ms * (0.7 + this.rand() * 0.6));
            this.history.push({
                label: d.label,
                class: d.cls,
                cwd: d.cwd,
                ticket: this.ticket++,
                queued_at_ms: this.now - (40 - i) * 60_000,
                wait_ms: Math.round(this.rand() * 90_000),
                run_ms: run,
                outcome: 'exit',
                exit: this.rand() < 0.12 ? 1 : 0,
            });
        }
        for (let i = 0; i < 9; i++) this.arrive();
        this.admit();
    }

    private rand(): number {
        // mulberry32
        let t = (this.seed += 0x6d2b79f5);
        t = Math.imul(t ^ (t >>> 15), t | 1);
        t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
        return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
    }

    private arrive(): void {
        const d = DEMO_JOBS[Math.floor(this.rand() * DEMO_JOBS.length)];
        const t = this.ticket++;
        this.waiting.push({
            class: d.cls,
            ticket: t,
            label: d.label,
            pid: 40_000 + t,
            cwd: d.cwd,
            queued_at_ms: this.now,
            locks: d.cargo ? [CARGO_LOCK] : undefined,
        });
    }

    private holds(): string[] {
        const out: string[] = [];
        if (this.swapUsed / 23.5 > 0.9 && this.memAvail < 10) out.push(`swap ${Math.round((this.swapUsed * 100) / 23.5)}% > 90% while memory is tight`);
        if (this.load / 12 > 4) out.push(`load ${(this.load / 12).toFixed(1)}/core > 4/core`);
        return out;
    }

    private admit(): void {
        const holds = this.holds();
        const heldLocks = new Set(this.running.flatMap((j) => j.locks ?? []));
        for (const cls of ['heavy', 'light'] as const) {
            const slots = cls === 'heavy' ? 2 : 6;
            const line = this.waiting.filter((w) => w.class === cls).sort((a, b) => a.ticket - b.ticket);
            for (const w of line) {
                const busy = this.running.filter((r) => r.class === cls);
                const lock = (w.locks ?? []).find((l) => heldLocks.has(l));
                if (lock) {
                    const holder = this.running.find((r) => (r.locks ?? []).includes(lock));
                    w.waiting_on = `${shortLock(lock)} held by #${holder?.ticket} ${holder?.label}`;
                    w.blocked_by_ticket = holder?.ticket;
                    continue; // a lock-blocked waiter does not hold up the line
                }
                if (busy.length >= slots) {
                    w.waiting_on = `${busy.length}/${slots} ${cls} busy`;
                    w.blocked_by_ticket = undefined;
                    continue;
                }
                if (cls === 'heavy' && holds.length > 0 && busy.length > 0) {
                    w.waiting_on = holds[0];
                    w.blocked_by_ticket = undefined;
                    continue;
                }
                const used = new Set(busy.map((b) => b.slot));
                let slot = 1;
                while (used.has(slot)) slot++;
                this.waiting = this.waiting.filter((x) => x.ticket !== w.ticket);
                const d = DEMO_JOBS.find((x) => x.label === w.label);
                const job: JobInfo = { ...w, admitted_at_ms: this.now, slot, child_pid: w.pid + 1, waiting_on: undefined, blocked_by_ticket: undefined };
                this.running.push(job);
                this.due.set(w.ticket, this.now + Math.round((d?.ms ?? 30_000) * (0.6 + this.rand() * 0.9)));
                for (const l of job.locks ?? []) heldLocks.add(l);
            }
        }
    }

    /** Advance the simulation and return a snapshot. */
    step(ms = 1000): Snapshot {
        this.now += ms;
        for (const j of [...this.running]) {
            if ((this.due.get(j.ticket) ?? Infinity) <= this.now) {
                this.running = this.running.filter((r) => r.ticket !== j.ticket);
                this.history.push({
                    label: j.label,
                    class: j.class,
                    cwd: j.cwd,
                    ticket: j.ticket,
                    queued_at_ms: j.queued_at_ms,
                    wait_ms: (j.admitted_at_ms ?? j.queued_at_ms) - j.queued_at_ms,
                    run_ms: this.now - (j.admitted_at_ms ?? this.now),
                    outcome: 'exit',
                    exit: this.rand() < 0.15 ? 1 : 0,
                });
                if (this.history.length > 200) this.history.shift();
            }
        }
        if (this.rand() < 0.3 * (ms / 1000) && this.waiting.length < 10) this.arrive();
        const heavy = this.running.filter((r) => r.class === 'heavy').length;
        const light = this.running.length - heavy;
        const targetLoad = 16 + heavy * 15 + light * 2.5 + this.rand() * 8;
        this.load += (targetLoad - this.load) * 0.08;
        this.memAvail += (18 - heavy * 5 - this.memAvail) * 0.05 + (this.rand() - 0.5);
        this.memAvail = Math.max(3, Math.min(40, this.memAvail));
        this.swapUsed += (heavy >= 2 ? 0.03 : -0.06) * (ms / 1000);
        this.swapUsed = Math.max(12, Math.min(22.6, this.swapUsed));
        this.admit();
        return this.snapshot();
    }

    snapshot(): Snapshot {
        const total = 24 * GB;
        return {
            schema: SCHEMA,
            dir: '/Users/dev/.smooth/ci-queue',
            config: {
                slots: { heavy: 2, light: 6 },
                gate: { min_available_memory_pct: 5, max_memory_pressure_level: 1, max_swap_used_pct: 90, max_load_per_core: 4, min_free_disk_gb: 20 },
            },
            now_ms: this.now,
            running: this.running.map((j) => ({ ...j })),
            waiting: this.waiting.map((j) => ({ ...j })).sort((a, b) => a.ticket - b.ticket),
            readings: {
                mem_total_bytes: total,
                mem_available_bytes: Math.round((total * this.memAvail) / 100),
                memory_pressure_level: this.memAvail < 8 ? 2 : 1,
                swap_total_bytes: Math.round(23.5 * GB),
                swap_used_bytes: Math.round(this.swapUsed * GB),
                load1: this.load,
                cores: 12,
                disks: [{ path: '/Users/dev/.cargo/shared-target', free_bytes: 142 * GB }],
            },
            holds: this.holds(),
            history: this.history.slice(-200),
        };
    }

    /** `t_ms` + readings, for sparklines. */
    sample(): Sample {
        return { t_ms: this.now, readings: this.snapshot().readings };
    }
}

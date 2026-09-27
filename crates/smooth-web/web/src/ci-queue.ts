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
    // Schema 2 (capacity-aware admission): what admission expects the job
    // to need, and — while it runs — its process group's memory right now.
    est?: Estimate;
    rss_now_kb?: number;
}

/** Schema 2: admission's estimate for a job (from its label's history, or the
 * class default when `from_runs` is 0). CPU is in millicores. */
export interface Estimate {
    rss_kb: number;
    millicores: number;
    from_runs: number;
}

export interface HistoryEntry {
    label: string;
    /** Null for rows older than the class column (schema 2 reads SQLite). */
    class: JobClass | null;
    cwd: string;
    ticket: number;
    queued_at_ms: number;
    wait_ms: number;
    run_ms: number;
    /** `exit`, `signal`, `timeout`, `spawn-failed`, or `wait-timeout`. */
    outcome: string;
    exit: number;
    // Schema 2: what the job actually cost. `peak_group_rss_kb` is the sum
    // across its process group at its peak; `max_single_rss_kb` the largest
    // one process — the gap between them is how parallel the job ran.
    peak_group_rss_kb?: number;
    max_single_rss_kb?: number;
    cpu_ms?: number;
    est_rss_kb?: number;
    est_millicores?: number;
    repo?: string;
    cmd_hash?: string;
    finished_at_ms?: number;
    /** The machine's readings when it was admitted. */
    pressure?: Readings;
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

/** Schema 2: where the capacity budget stands. `scale` is the AIMD
 * multiplier (+step while calm, ×0.5 on a gate signal).
 * - `mem_pool_kb`: memory jobs may be admitted into, (available − reserve) ×
 *   min(scale, mem_scale_max). Already effective; null when unreadable.
 * - `mem_committed_kb`: what running jobs may still grow into, Σ(est − now).
 * - `cpu_budget_millicores` (already scaled) and `cpu_used_millicores`: Σ est. */
export interface Budget {
    scale: number;
    mem_pool_kb: number | null;
    mem_committed_kb: number;
    cpu_budget_millicores: number;
    cpu_used_millicores: number;
}

/** Schema 2: `config.budget` — only what this page reads. */
export interface BudgetConfig {
    enabled?: boolean;
    cpu_factor?: number;
    mem_reserve_gb?: number;
    aimd_min?: number;
    aimd_max?: number;
    mem_scale_max?: number;
}

export interface Snapshot {
    schema?: number;
    budget?: Budget;
    /** Set when the OS has no queue (Windows): `{schema, unsupported}`. */
    unsupported?: string;
    dir: string;
    config: { slots: { heavy: number; light: number }; gate: Gate; budget?: BudgetConfig };
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
    /** The budget at that moment, when the queue has one (schema 2). */
    budget?: Budget;
}

/** `GET /api/ci-queue/status`. */
export interface StatusResponse {
    snapshot: Snapshot | null;
    samples: Sample[];
    /** Set when the daemon could not read the queue — shown, never hidden. */
    error?: string;
}

/** The newest schema this client knows. Unknown fields are ignored and every
 * panel checks for its own fields, so a newer snapshot still renders. */
export const SCHEMA = 2;

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
        const free = d.free_bytes / GB;
        if (!best || free < best.gb) best = { gb: free, path: d.path };
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
        if (holder) return `lock ${shortLock(lock)} held by ${holder.label} (#${holder.ticket})`;
    }
    const busy = snap.running.filter((r) => r.class === j.class).length;
    const slots = snap.config.slots[j.class];
    if (busy >= slots) return `${busy}/${slots} ${j.class} busy`;
    if (j.class === 'heavy' && snap.holds.length > 0) return snap.holds[0];
    const ahead = snap.waiting.filter((w) => w.class === j.class && w.ticket < j.ticket).length;
    return ahead > 0 ? `${ahead} ahead in line` : 'next in line';
}

export type ReasonKind = 'lock' | 'line' | 'budget' | 'gate';

/** The queue joins a reason's parts with " / ", most specific first
 * (`lock cargo:… held by clippy (#412) / 2 heavy busy: …`). The first part is
 * the one that matters; the rest is context. */
export function reasonHead(reason: string): string {
    return reason.split(' / ')[0] ?? reason;
}

/** What kind of thing holds a waiter back: a neighbour's lock, just the line
 * (slots busy, others ahead), the admission budget, or the pressure gate. */
export function reasonKind(reason: string): ReasonKind {
    const head = reasonHead(reason);
    if (/^lock |held by|building outside the queue/.test(head)) return 'lock';
    if (/^(memory|cpu) budget/.test(head)) return 'budget';
    if (/busy|ahead|next in line/.test(head)) return 'line';
    return 'gate';
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
    return lock.replace(/\/(?:Users|home)\/[^/]+/g, '~');
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

// ── Budget (schema 2) ────────────────────────────────────────────────────────

export interface BudgetSlice {
    job: JobInfo;
    /** What admission reserved for it. */
    estKb: number;
    /** Its process group's memory now; null until the first sample. */
    nowKb: number | null;
    /** What it may still grow into: est − now (never negative). */
    committedKb: number;
    millicores: number;
}

/** Each running job's slice of the budget, biggest estimate first. */
export function budgetSlices(snap: Snapshot): BudgetSlice[] {
    return snap.running
        .filter((j) => j.est != null)
        .map((j) => {
            const estKb = j.est?.rss_kb ?? 0;
            const nowKb = j.rss_now_kb ?? null;
            return { job: j, estKb, nowKb, committedKb: Math.max(0, estKb - (nowKb ?? 0)), millicores: j.est?.millicores ?? 0 };
        })
        .sort((a, b) => b.estKb - a.estKb);
}

export function gb(kb: number): string {
    const g = kb / 1_048_576;
    return g >= 10 ? `${g.toFixed(0)} GB` : `${g.toFixed(1)} GB`;
}

export interface LabelCost {
    label: string;
    runs: number;
    p50RunMs: number;
    p50PeakKb: number;
    /** The largest single process, when known: the gap to the peak is parallelism. */
    p50SingleKb: number | null;
    p50CpuMs: number;
    /** cpu time ÷ wall time: how many cores it keeps busy. */
    cores: number;
}

function median(v: number[]): number {
    const s = [...v].sort((a, b) => a - b);
    const m = Math.floor(s.length / 2);
    return s.length % 2 ? s[m] : (s[m - 1] + s[m]) / 2;
}

/** Per-label cost profiles from history that carries rusage (schema 2). */
export function labelCosts(history: HistoryEntry[]): LabelCost[] {
    const by = new Map<string, HistoryEntry[]>();
    for (const h of history) {
        if (h.peak_group_rss_kb == null || h.run_ms <= 0) continue;
        const list = by.get(h.label) ?? [];
        list.push(h);
        by.set(h.label, list);
    }
    return [...by.entries()]
        .map(([label, hs]) => {
            const p50RunMs = median(hs.map((h) => h.run_ms));
            const p50CpuMs = median(hs.map((h) => h.cpu_ms ?? 0));
            const singles = hs.map((h) => h.max_single_rss_kb).filter((v): v is number => v != null);
            return {
                label,
                runs: hs.length,
                p50RunMs,
                p50PeakKb: median(hs.map((h) => h.peak_group_rss_kb ?? 0)),
                p50SingleKb: singles.length ? median(singles) : null,
                p50CpuMs,
                cores: p50RunMs > 0 ? p50CpuMs / p50RunMs : 0,
            };
        })
        .sort((a, b) => b.p50PeakKb - a.p50PeakKb);
}

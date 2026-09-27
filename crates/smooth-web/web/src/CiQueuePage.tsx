//! The check queue, live — `th ci-queue web` and Big Smooth's Queue tab
//! (SMOODEV-3371).
//!
//! One picture of the machine: waiting jobs line up on the left, pass through
//! the gate, and run in the heavy and light slot lanes on the right, while a
//! three.js stream of work flows through it (QueueFlow). The gate is the one
//! bold element: it glows with the machine's heat (Aurora spectrum, teal →
//! gold → coral) and closes when the pressure gate holds heavy jobs, and the
//! same gate line runs down through the pressure gauges, because every gauge
//! draws its threshold at the same x. Locks glow gold while held.
//!
//! Below the machine: pressure against thresholds, the admission budget
//! (schema 2: each job's estimated vs actual slice, and the AIMD scale over
//! time), who holds which lock, per-label cost profiles, and recent jobs.
//! Every panel checks for its own fields, so an older `th` simply shows fewer.

import { Activity, ChevronRight, Cpu, History, Lock, Pause, Play, ScatterChart, Wifi, WifiOff } from 'lucide-react';
import { useMemo, useState, type CSSProperties, type ReactNode } from 'react';

import {
    budgetSlices,
    duration,
    gaugeFill,
    GATE_X,
    gb,
    HEAT,
    heatOf,
    labelCosts,
    labelP50s,
    lanes,
    lockHolders,
    machineHeat,
    progress,
    reasonHead,
    reasonKind,
    SCHEMA,
    shortLock,
    signals,
    signalSeries,
    waitReason,
    worktreeOf,
    type Budget,
    type HistoryEntry,
    type JobInfo,
    type LabelCost,
    type Sample,
    type Signal,
    type Snapshot,
} from './ci-queue';
import { PERIOD_S } from './ci-queue-demo';
import { QueueFlow } from './components/QueueFlow';
import { useCiQueue, type Source } from './useCiQueue';

const OK = '#34d399';

const transitionName = (ticket: number): CSSProperties => ({ viewTransitionName: `ciq-${ticket}` }) as CSSProperties;

// ── The machine ─────────────────────────────────────────────────────────────

const TONE = { lock: HEAT[3], line: 'var(--ciq-muted)', budget: HEAT[4], gate: HEAT[5] } as const;

function Waiter({ job, snap, selected, onSelect }: { job: JobInfo; snap: Snapshot; selected: boolean; onSelect: () => void }) {
    const reason = waitReason(job, snap);
    const kind = reasonKind(reason);
    return (
        <li style={transitionName(job.ticket)} data-ciq-wait>
            <button
                type="button"
                onClick={onSelect}
                aria-pressed={selected}
                className={`relative w-full rounded-xl border px-3 py-2 text-left backdrop-blur-[2px] transition ${selected ? 'border-(--ciq-heat)/60 bg-white/6' : 'border-(--ciq-border) bg-[#0a0f1e]/70 hover:border-white/15'}`}
            >
                <div className="flex items-baseline gap-2">
                    <span className="font-mono text-[11px] text-(--ciq-faint) tabular-nums">#{job.ticket}</span>
                    <span className="min-w-0 flex-1 truncate text-[13px] text-(--ciq-text)">{job.label}</span>
                    <span className="text-[11px] text-(--ciq-muted) tabular-nums">{duration(snap.now_ms - job.queued_at_ms)}</span>
                </div>
                <div className="mt-0.5 flex items-center gap-1.5 text-[11.5px]" style={{ color: TONE[kind] }}>
                    {kind === 'lock' && <Lock className="size-3 shrink-0" aria-label="waiting on a lock" />}
                    <span className="truncate" title={reason}>
                        {shortLock(reasonHead(reason))}
                    </span>
                </div>
            </button>
        </li>
    );
}

function RunningJob({
    job,
    snap,
    p50s,
    compact,
    selected,
    onSelect,
}: {
    job: JobInfo;
    snap: Snapshot;
    p50s: Map<string, number>;
    compact?: boolean;
    selected: boolean;
    onSelect: () => void;
}) {
    const p = progress(job, snap.now_ms, p50s);
    const p50 = p50s.get(job.label);
    const fill = p.over ? HEAT[3] : 'var(--ciq-heat)';
    const locked = (job.locks?.length ?? 0) > 0;
    return (
        <button
            type="button"
            onClick={onSelect}
            aria-pressed={selected}
            style={transitionName(job.ticket)}
            data-ciq-run
            data-lock={locked ? '1' : '0'}
            data-frac={(p.frac ?? 0.5).toFixed(3)}
            className={`relative block w-full overflow-hidden rounded-xl border bg-[#0a0f1e]/60 text-left ${locked ? 'ciq-lock-held' : ''} ${selected ? 'border-(--ciq-heat)/70' : 'border-white/10'}`}
        >
            {/* elapsed ÷ this label's p50 here; indeterminate when it has never finished */}
            {p.frac == null ? (
                <span className="ciq-indeterminate absolute inset-y-0 left-0 w-full" aria-hidden />
            ) : (
                <span
                    className="absolute inset-y-0 left-0 transition-[width] duration-1000 ease-linear"
                    style={{ width: `${p.frac * 100}%`, background: `linear-gradient(90deg, transparent, color-mix(in oklch, ${fill} 30%, transparent))` }}
                    aria-hidden
                />
            )}
            <span
                className="absolute bottom-0 left-0 h-[2px] transition-[width] duration-1000 ease-linear"
                style={{ width: `${(p.frac ?? 0) * 100}%`, background: fill, opacity: p.frac == null ? 0 : 0.9 }}
                aria-hidden
            />
            <span className={`relative flex items-center gap-2 ${compact ? 'px-2.5 py-1.5' : 'px-3 py-2.5'}`}>
                {locked && <Lock className="size-3.5 shrink-0" style={{ color: HEAT[3] }} aria-label="holds a lock" />}
                <span className="min-w-0 flex-1">
                    <span className={`block truncate text-(--ciq-text) ${compact ? 'text-[12px]' : 'text-[13.5px] font-medium'}`}>{job.label}</span>
                    {!compact && <span className="block truncate text-[11px] text-(--ciq-faint)">{worktreeOf(job.cwd)}</span>}
                </span>
                <span className={`shrink-0 text-right tabular-nums ${compact ? 'text-[11px]' : 'text-[12px]'}`}>
                    <span style={{ color: p.over ? HEAT[3] : 'var(--ciq-text)' }}>{duration(p.elapsedMs)}</span>
                    {!compact && <span className="text-(--ciq-faint)"> / {p50 ? duration(p50) : 'new'}</span>}
                </span>
            </span>
        </button>
    );
}

function FreeSlot({ compact, label }: { compact?: boolean; label: string }) {
    return (
        <div
            className={`flex items-center rounded-xl border border-dashed border-white/8 text-(--ciq-faint) ${compact ? 'px-2.5 py-1.5 text-[11px]' : 'px-3 py-[15px] text-[12px]'}`}
        >
            {label}
        </div>
    );
}

/** The gate — the one bold element. Its colour is the machine's heat; when the
 * pressure gate holds heavy jobs it closes into a dashed bar. */
function Gate({ holding }: { holding: boolean }) {
    return (
        <div className="relative z-10 flex items-stretch justify-center md:w-10" aria-hidden>
            <div className={`ciq-gate ${holding ? 'ciq-gate--held' : ''}`} data-ciq-gate />
            {!holding && (
                <div className="pointer-events-none absolute inset-0 hidden md:block">
                    <ChevronRight className="ciq-flow-chev absolute left-1/2 size-4 -translate-x-1/2" />
                </div>
            )}
        </div>
    );
}

function Detail({ job, snap, p50s }: { job: JobInfo; snap: Snapshot; p50s: Map<string, number> }) {
    const running = job.admitted_at_ms != null;
    const rows: Array<[string, ReactNode]> = [
        ['Ticket', `#${job.ticket} · ${job.class}${running && job.slot != null ? ` slot ${job.slot}` : ''}`],
        [
            'Worktree',
            <span key="cwd" className="break-all">
                {job.cwd}
            </span>,
        ],
        ['Process', `th pid ${job.pid}${job.child_pid ? ` · job pid ${job.child_pid}` : ''}`],
        ['Queued', new Date(job.queued_at_ms).toLocaleTimeString()],
    ];
    if (running) {
        const p50 = p50s.get(job.label);
        rows.push(['Running', `${duration(snap.now_ms - (job.admitted_at_ms ?? 0))} (usually ${p50 ? duration(p50) : 'unknown — first run here'})`]);
        if (job.est_rss_kb != null) {
            rows.push([
                'Budgeted',
                `${gb(job.est_rss_kb)}${job.est_cores != null ? ` · ${job.est_cores.toFixed(1)} cores` : ''}${job.rss_kb != null ? ` — using ${gb(job.rss_kb)}` : ''}`,
            ]);
        }
    } else {
        const reason = waitReason(job, snap);
        rows.push([
            'Waiting on',
            <span key="why" className="break-words">
                {reason}
            </span>,
        ]);
        if (job.waiting_on_since_ms) rows.push(['Since', duration(snap.now_ms - job.waiting_on_since_ms)]);
    }
    if (job.locks?.length) rows.push(['Locks', job.locks.map(shortLock).join(', ')]);
    return (
        <dl className="grid grid-cols-[6.5rem_minmax(0,1fr)] gap-x-4 gap-y-1.5 text-[12.5px]">
            {rows.map(([k, v]) => (
                <div key={k} className="contents">
                    <dt className="text-(--ciq-faint)">{k}</dt>
                    <dd className="text-(--ciq-text) tabular-nums">{v}</dd>
                </div>
            ))}
        </dl>
    );
}

// ── Panels ──────────────────────────────────────────────────────────────────

function Panel({
    icon,
    title,
    aside,
    children,
    className = '',
}: {
    icon: ReactNode;
    title: string;
    aside?: ReactNode;
    children: ReactNode;
    className?: string;
}) {
    return (
        <section className={`min-w-0 rounded-[18px] border border-(--ciq-border) bg-(--ciq-card) p-4 ${className}`}>
            <h2 className="mb-2 flex items-center gap-2 text-[13px] font-semibold text-(--ciq-text)">
                <span className="text-(--ciq-heat)">{icon}</span>
                {title}
                {aside && <span className="ml-auto text-right text-[11.5px] font-normal text-(--ciq-faint)">{aside}</span>}
            </h2>
            {children}
        </section>
    );
}

function Sparkline({ series, heat }: { series: Array<number | null>; heat: string }) {
    const w = 120;
    const h = 28;
    const pts = series
        .map((v, i) => (v == null ? null : `${((i / Math.max(1, series.length - 1)) * w).toFixed(1)},${(h - gaugeFill(v) * h).toFixed(1)}`))
        .filter(Boolean)
        .join(' ');
    const gateY = h - GATE_X * h;
    return (
        <svg viewBox={`0 0 ${w} ${h}`} className="h-7 w-[120px] shrink-0 overflow-visible" role="img" aria-label="last ten minutes">
            <line x1="0" x2={w} y1={gateY} y2={gateY} stroke="var(--ciq-gate)" strokeOpacity="0.45" strokeDasharray="2 3" />
            {pts && <polyline points={pts} fill="none" stroke={heat} strokeWidth="1.5" strokeLinejoin="round" strokeLinecap="round" />}
        </svg>
    );
}

function Gauge({ s, series }: { s: Signal; series: Array<number | null> }) {
    const heat = s.off || s.ratio == null ? 'var(--ciq-faint)' : HEAT[heatOf(s.ratio)];
    const fill = gaugeFill(s.ratio);
    return (
        <div className="grid grid-cols-[minmax(0,1fr)] items-center gap-x-4 gap-y-1 pt-2 pb-5 sm:grid-cols-[9.5rem_minmax(0,1fr)_120px] sm:pb-4">
            <div className="flex items-baseline justify-between gap-2 sm:block">
                <div className="text-[13px] text-(--ciq-text)">{s.name}</div>
                <div className="text-[12px] tabular-nums" style={{ color: heat }}>
                    {s.value}
                    {s.off && <span className="text-(--ciq-faint)"> · off</span>}
                </div>
            </div>
            <div className="relative h-2.5 rounded-full bg-white/6" title={`${s.limit}${s.note ? ` (${s.note})` : ''}`}>
                <div className="absolute inset-y-0 left-0 rounded-full transition-[width] duration-700" style={{ width: `${fill * 100}%`, background: heat }} />
                {/* the gate line: every gauge's threshold sits at the same x */}
                <div className="ciq-gate-tick absolute -inset-y-1.5 w-[2px] rounded-full" style={{ left: `calc(${GATE_X * 100}% - 1px)` }} />
                <div className="absolute top-3.5 text-[10.5px] whitespace-nowrap text-(--ciq-faint)" style={{ right: `${(1 - GATE_X) * 100}%` }}>
                    {s.limit}
                    {s.note ? ` · ${s.note}` : ''}
                </div>
            </div>
            <div className="hidden sm:block">
                <Sparkline series={series} heat={heat} />
            </div>
        </div>
    );
}

/** One budget meter: each job's estimated slice as an outline, what it is
 * actually using as the fill inside it (coral past its estimate), the
 * effective budget as the heat line and the unscaled base as a dashed one. */
function BudgetMeter({
    title,
    used,
    budget,
    base,
    slices,
    fmt,
}: {
    title: string;
    used: number;
    budget: number;
    base: number;
    slices: Array<{ key: number; label: string; est: number; actual: number | null }>;
    fmt: (n: number) => string;
}) {
    const max = Math.max(base * 1.3, used * 1.08, budget * 1.05, 1e-9);
    const pct = (n: number) => `${(n / max) * 100}%`;
    const over = used > budget;
    return (
        <div className="py-2">
            <div className="mb-1.5 flex items-baseline justify-between gap-3 text-[12.5px]">
                <span className="text-(--ciq-text)">{title}</span>
                <span className="text-(--ciq-muted) tabular-nums">
                    <span style={{ color: over ? HEAT[5] : 'var(--ciq-text)' }}>{fmt(used)}</span> of {fmt(budget)} budgeted
                </span>
            </div>
            <div className="relative h-6 rounded-md bg-white/5">
                <div className="absolute inset-y-0 left-0 flex gap-[2px]" style={{ width: pct(used) }}>
                    {slices.map((s) => {
                        const inner = s.actual == null ? 1 : Math.min(1, s.actual / Math.max(s.est, 1e-9));
                        const past = s.actual != null && s.actual > s.est;
                        return (
                            <div
                                key={s.key}
                                className="relative h-full min-w-[3px] rounded-[4px] border border-(--ciq-heat)/55"
                                style={{ flexGrow: s.est, flexBasis: 0 }}
                                title={`${s.label} — budgeted ${fmt(s.est)}${s.actual != null ? `, using ${fmt(s.actual)}` : ''}`}
                            >
                                <div
                                    className="absolute inset-y-0 left-0 rounded-[3px] transition-[width] duration-700"
                                    style={{ width: `${inner * 100}%`, background: past ? HEAT[5] : 'color-mix(in oklch, var(--ciq-heat) 55%, transparent)' }}
                                />
                            </div>
                        );
                    })}
                </div>
                <div
                    className="absolute -inset-y-1 w-0 border-l border-dashed border-white/35"
                    style={{ left: pct(base) }}
                    title={`base budget ${fmt(base)}`}
                />
                <div
                    className="ciq-gate-tick absolute -inset-y-1.5 w-[2px] rounded-full transition-[left] duration-700"
                    style={{ left: `calc(${pct(budget)} - 1px)` }}
                />
            </div>
        </div>
    );
}

/** The AIMD budget scale over the sample window: it climbs a step at a time
 * while pressure stays low and halves on a spike — the sawtooth. */
function ScaleChart({ samples, current }: { samples: Sample[]; current: Budget }) {
    const [hover, setHover] = useState<number | null>(null);
    const pts = samples.filter((s) => s.budget).map((s) => ({ t: s.t_ms, v: s.budget?.scale ?? 0 }));
    const w = 420;
    const h = 96;
    const top = Math.max(current.scale_max ?? 1.25, ...pts.map((p) => p.v)) * 1.05;
    const x = (i: number) => (i / Math.max(1, pts.length - 1)) * w;
    const y = (v: number) => h - (v / top) * h;
    const path = pts.map((p, i) => `${i ? 'L' : 'M'}${x(i).toFixed(1)},${y(p.v).toFixed(1)}`).join('');
    const area = pts.length ? `${path}L${x(pts.length - 1).toFixed(1)},${h}L0,${h}Z` : '';
    const hv = hover != null ? pts[hover] : null;
    return (
        <div className="mt-2">
            <div className="mb-1 flex items-baseline justify-between text-[12.5px]">
                <span className="text-(--ciq-text)">Budget scale, last 10 min</span>
                <span className="text-(--ciq-muted) tabular-nums">
                    ×{(hv?.v ?? current.scale).toFixed(2)}
                    {hv ? ` at ${new Date(hv.t).toLocaleTimeString()}` : ' now'}
                </span>
            </div>
            <svg
                viewBox={`0 0 ${w} ${h}`}
                className="h-24 w-full overflow-visible"
                preserveAspectRatio="none"
                role="img"
                aria-label="budget scale over time"
                onMouseMove={(e) => {
                    const r = e.currentTarget.getBoundingClientRect();
                    setHover(Math.round(((e.clientX - r.left) / r.width) * Math.max(0, pts.length - 1)));
                }}
                onMouseLeave={() => setHover(null)}
            >
                <line x1="0" x2={w} y1={y(1)} y2={y(1)} stroke="white" strokeOpacity="0.25" strokeDasharray="3 4" vectorEffect="non-scaling-stroke" />
                {area && <path d={area} fill="var(--ciq-heat)" fillOpacity="0.12" />}
                {path && <path d={path} fill="none" stroke="var(--ciq-heat)" strokeWidth="2" strokeLinejoin="round" vectorEffect="non-scaling-stroke" />}
                {hover != null && hv && <line x1={x(hover)} x2={x(hover)} y1="0" y2={h} stroke="white" strokeOpacity="0.4" vectorEffect="non-scaling-stroke" />}
            </svg>
            <div className="mt-1 flex justify-between text-[10.5px] text-(--ciq-faint)">
                <span>grows while calm, halves on a spike</span>
                <span>dashed = ×1 (base budget)</span>
            </div>
        </div>
    );
}

function BudgetPanel({ snap, samples }: { snap: Snapshot; samples: Sample[] }) {
    const b = snap.budget;
    if (!b) return null;
    const slices = budgetSlices(snap);
    const scale = b.scale > 0 ? b.scale : 1;
    return (
        <Panel icon={<Cpu className="size-4" />} title="Admission budget" aside={`×${b.scale.toFixed(2)} of base`}>
            <BudgetMeter
                title="Memory"
                used={b.mem_used_kb}
                budget={b.mem_kb}
                base={b.mem_kb / scale}
                fmt={gb}
                slices={slices.map((s) => ({ key: s.job.ticket, label: s.job.label, est: s.estKb, actual: s.actualKb }))}
            />
            <BudgetMeter
                title="CPU"
                used={b.cores_used}
                budget={b.cores}
                base={b.cores / scale}
                fmt={(n) => `${n.toFixed(1)} cores`}
                slices={slices.map((s) => ({ key: s.job.ticket, label: s.job.label, est: s.estCores, actual: s.actualCores }))}
            />
            <ScaleChart samples={samples} current={b} />
        </Panel>
    );
}

/** Per-label cost: typical run time (x) against typical peak memory (y), dot
 * size = cores it keeps busy. One series, so no legend; labels are direct. */
function CostScatter({ costs }: { costs: LabelCost[] }) {
    const w = 460;
    const h = 240;
    const pad = { l: 44, r: 12, t: 12, b: 30 };
    const maxX = Math.max(...costs.map((c) => c.p50RunMs), 1) * 1.08;
    const maxY = Math.max(...costs.map((c) => c.p50PeakKb), 1) * 1.15;
    const X = (v: number) => pad.l + (v / maxX) * (w - pad.l - pad.r);
    const Y = (v: number) => h - pad.b - (v / maxY) * (h - pad.t - pad.b);
    // Direct labels: to the left of dots in the right third, and nudged down
    // where two would collide.
    const placed: Array<{ c: LabelCost; cx: number; cy: number; r: number; lx: number; ly: number; anchor: 'start' | 'end'; text: string }> = [];
    for (const c of [...costs].sort((a, b) => b.p50PeakKb - a.p50PeakKb)) {
        const cx = X(c.p50RunMs);
        const cy = Y(c.p50PeakKb);
        const r = 4 + Math.min(8, c.cores * 1.2);
        const text = c.label.length > 22 ? `${c.label.slice(0, 21)}…` : c.label;
        const anchor = cx > w * 0.6 ? 'end' : 'start';
        const lx = anchor === 'end' ? cx - r - 4 : cx + r + 4;
        const tw = text.length * 5.8;
        const span = (x: number, a: 'start' | 'end') => (a === 'end' ? [x - tw, x] : [x, x + tw]);
        let ly = cy + 3.5;
        for (const p of placed) {
            const [a0, a1] = span(lx, anchor);
            const [b0, b1] = span(p.lx, p.anchor);
            if (a0 < b1 && b0 < a1 && Math.abs(ly - p.ly) < 12) ly = p.ly + 12;
        }
        placed.push({ c, cx, cy, r, lx, ly, anchor, text });
    }
    return (
        <svg viewBox={`0 0 ${w} ${h}`} className="h-auto w-full" role="img" aria-label="cost per check: run time against peak memory">
            {[0, maxY / 2, maxY].map((v) => (
                <g key={`y${v}`}>
                    <line x1={pad.l} x2={w - pad.r} y1={Y(v)} y2={Y(v)} stroke="white" strokeOpacity="0.06" />
                    <text x={pad.l - 6} y={Y(v) + 3} textAnchor="end" className="fill-(--ciq-faint) text-[10px] tabular-nums">
                        {gb(v)}
                    </text>
                </g>
            ))}
            {[0, maxX / 2, maxX].map((v) => (
                <text key={`x${v}`} x={X(v)} y={h - pad.b + 16} textAnchor="middle" className="fill-(--ciq-faint) text-[10px] tabular-nums">
                    {duration(v)}
                </text>
            ))}
            {placed.map((p) => (
                <circle key={`d${p.c.label}`} cx={p.cx} cy={p.cy} r={p.r} fill="var(--ciq-heat)" fillOpacity="0.55" stroke="#0e1526" strokeWidth="2">
                    <title>{`${p.c.label}: ${duration(p.c.p50RunMs)} typical, peak ${gb(p.c.p50PeakKb)}, ${p.c.cores.toFixed(1)} cores busy, ${p.c.runs} runs`}</title>
                </circle>
            ))}
            {placed.map((p) => (
                <text key={`l${p.c.label}`} x={p.lx} y={p.ly} textAnchor={p.anchor} className="fill-(--ciq-muted) text-[10.5px]">
                    {p.text}
                </text>
            ))}
        </svg>
    );
}

function HistoryRow({ h, live }: { h: HistoryEntry; live: boolean }) {
    const ok = h.outcome === 'exit' && h.exit === 0;
    const tone = ok ? OK : h.outcome === 'wait-timeout' ? HEAT[3] : HEAT[5];
    const what = ok ? 'passed' : h.outcome === 'exit' ? `exit ${h.exit}` : h.outcome === 'signal' ? `signal ${h.exit - 128}` : h.outcome;
    return (
        <li
            style={live ? transitionName(h.ticket) : undefined}
            className="grid grid-cols-[4.5rem_minmax(0,1fr)] items-baseline gap-x-3 py-1.5 text-[12.5px] sm:grid-cols-[4.5rem_minmax(0,1fr)_auto]"
        >
            <span className="flex items-center gap-1.5 tabular-nums" style={{ color: tone }}>
                <span className="size-1.5 rounded-full" style={{ background: tone }} />
                {what}
            </span>
            <span className="min-w-0 truncate text-(--ciq-text)">
                {h.label} <span className="text-(--ciq-faint)">· {worktreeOf(h.cwd)}</span>
            </span>
            <span className="col-start-2 text-[11.5px] whitespace-nowrap text-(--ciq-muted) tabular-nums sm:col-start-auto sm:text-right">
                waited {duration(h.wait_ms)} · ran {duration(h.run_ms)}
            </span>
        </li>
    );
}

// ── Page ────────────────────────────────────────────────────────────────────

/** The queue view. `source` picks the feed: `stream` (SSE from `th ci-queue
 * web`) or `daemon` (Big Smooth's polled route). `standalone` gives it the
 * whole window. */
export default function CiQueuePage({ source = 'daemon', standalone = false }: { source?: Source; standalone?: boolean }) {
    const { snap, samples, error, stale, demo, paused, setPaused } = useCiQueue(source);
    const [selected, setSelected] = useState<number | null>(null);
    const [chamber, setChamber] = useState<HTMLElement | null>(null);
    const p50s = useMemo(() => labelP50s(snap?.history ?? []), [snap?.history]);
    const costs = useMemo(() => labelCosts(snap?.history ?? []), [snap?.history]);

    const heat = snap ? machineHeat(snap) : 0;
    const style = { '--ciq-heat': HEAT[heat] } as CSSProperties;
    const pad = standalone ? 'px-4 pb-6 pt-4 sm:px-6' : 'pt-[calc(env(safe-area-inset-top)+4rem)] pb-6 sm:pt-6';

    if (!snap) {
        return (
            <main className={`ciq flex min-h-0 flex-1 flex-col items-center justify-center ${pad}`} style={style}>
                <p className="max-w-md text-center text-sm text-(--ciq-muted)">{error ?? 'Reading the queue…'}</p>
            </main>
        );
    }

    const holding = snap.holds.length > 0;
    const heavy = lanes(snap, 'heavy');
    const light = lanes(snap, 'light');
    const busyHeavy = heavy.filter(Boolean).length;
    const busyLight = light.filter(Boolean).length;
    const sigs = signals(snap.readings, snap.config.gate);
    const locks = lockHolders(snap);
    const recent = [...snap.history].reverse().slice(0, standalone ? 10 : 8);
    const all = [...snap.running, ...snap.waiting];
    const sel = all.find((j) => j.ticket === selected) ?? null;
    const toggle = (t: number) => setSelected((s) => (s === t ? null : t));

    const verdict = holding
        ? `Holding heavy jobs — ${snap.holds[0]}`
        : snap.waiting.length === 0 && snap.running.length === 0
          ? 'Idle. The machine has room.'
          : busyHeavy >= snap.config.slots.heavy
            ? 'Heavy lanes full. Jobs wait their turn.'
            : 'Admitting jobs as slots free up';

    return (
        <main className={`ciq flex min-h-0 flex-1 flex-col gap-4 overflow-y-auto ${pad} [&>*]:shrink-0`} style={style}>
            {/* ── The machine ───────────────────────────────────────────── */}
            <section ref={setChamber} className="ciq-chamber relative overflow-hidden rounded-[18px] border border-white/10 p-4 sm:p-5">
                <div className="ciq-ambient" aria-hidden />
                <QueueFlow root={chamber} heat={HEAT[heat]} holding={holding} hot={HEAT[5]} gold={HEAT[3]} />
                <header className="relative z-10 flex flex-wrap items-start gap-x-6 gap-y-2">
                    <div className="min-w-0 flex-1">
                        <h1
                            className={`font-display leading-tight font-bold tracking-[-0.02em] text-balance text-(--ciq-text) ${standalone ? 'text-[24px] sm:text-[32px]' : 'text-[22px] sm:text-[26px]'}`}
                        >
                            {verdict}
                        </h1>
                        <p className="mt-1 flex flex-wrap items-center gap-x-2 text-[12.5px] text-(--ciq-muted)">
                            {demo ? (
                                <DemoStrip phase={demo.phase} t={demo.t} />
                            ) : (
                                <>
                                    <span>Heavy checks on this machine take a slot first.</span>
                                    {source === 'stream' && (
                                        <span className="flex items-center gap-1" style={{ color: stale ? HEAT[3] : 'var(--ciq-faint)' }}>
                                            {stale ? <WifiOff className="size-3.5" /> : <Wifi className="size-3.5" />}
                                            {stale ? 'reconnecting to th…' : 'live'}
                                        </span>
                                    )}
                                </>
                            )}
                        </p>
                    </div>
                    <div className="flex items-center gap-4 text-[12.5px] text-(--ciq-muted) tabular-nums">
                        <span>
                            <b className="font-display text-lg text-(--ciq-text)">{busyHeavy}</b>/{snap.config.slots.heavy} heavy
                        </span>
                        <span>
                            <b className="font-display text-lg text-(--ciq-text)">{busyLight}</b>/{snap.config.slots.light} light
                        </span>
                        <span>
                            <b className="font-display text-lg text-(--ciq-text)">{snap.waiting.length}</b> waiting
                        </span>
                        <button
                            type="button"
                            onClick={() => setPaused(!paused)}
                            className="flex items-center gap-1 rounded-full border border-white/10 px-2.5 py-1 text-[12px] text-(--ciq-muted) hover:bg-white/5"
                            aria-label={paused ? 'Resume live updates' : 'Pause live updates'}
                        >
                            {paused ? <Play className="size-3.5" /> : <Pause className="size-3.5" />}
                            {paused ? 'Resume' : 'Pause'}
                        </button>
                    </div>
                </header>
                {error && (
                    <p className="relative z-10 mt-3 rounded-xl bg-white/5 px-3 py-2 text-[12.5px] text-(--ciq-muted)">{error} Showing the last reading.</p>
                )}
                {snap.schema != null && snap.schema > SCHEMA && (
                    <p className="relative z-10 mt-3 text-[12px]" style={{ color: HEAT[3] }}>
                        This th reports a newer queue format ({snap.schema}); anything this page doesn't know yet is left out.
                    </p>
                )}

                <div
                    className={`relative z-10 mt-5 grid gap-4 md:grid-cols-[minmax(0,19rem)_auto_minmax(0,1fr)] md:gap-3 ${standalone ? 'lg:grid-cols-[minmax(0,24rem)_auto_minmax(0,1fr)]' : ''}`}
                >
                    <div>
                        <div className="mb-2 text-[12px] text-(--ciq-muted)">In line, first come first served</div>
                        {snap.waiting.length === 0 ? (
                            <p className="rounded-xl border border-dashed border-white/8 px-3 py-4 text-[12.5px] text-(--ciq-faint)">Nobody waiting.</p>
                        ) : (
                            <ol className={`flex flex-col gap-1.5 overflow-y-auto pr-1 ${standalone ? 'max-h-[30rem]' : 'max-h-[23rem]'}`}>
                                {snap.waiting.map((j) => (
                                    <Waiter key={j.ticket} job={j} snap={snap} selected={selected === j.ticket} onSelect={() => toggle(j.ticket)} />
                                ))}
                            </ol>
                        )}
                    </div>

                    <Gate holding={holding} />

                    <div className="flex min-w-0 flex-col gap-4">
                        <div>
                            <div className="mb-2 flex items-baseline gap-2 text-[12px] text-(--ciq-muted)">
                                Heavy <span className="text-(--ciq-faint)">typecheck, clippy, tests · held under pressure</span>
                            </div>
                            <div className="flex flex-col gap-1.5">
                                {heavy.map((j, i) =>
                                    j ? (
                                        <RunningJob
                                            key={j.ticket}
                                            job={j}
                                            snap={snap}
                                            p50s={p50s}
                                            selected={selected === j.ticket}
                                            onSelect={() => toggle(j.ticket)}
                                        />
                                    ) : (
                                        <FreeSlot key={`h${i}`} label={holding && busyHeavy > 0 ? 'free, but the gate is holding' : 'free'} />
                                    ),
                                )}
                            </div>
                        </div>
                        <div>
                            <div className="mb-2 flex items-baseline gap-2 text-[12px] text-(--ciq-muted)">
                                Light <span className="text-(--ciq-faint)">formatters, linters, guards</span>
                            </div>
                            <div className="grid grid-cols-2 gap-1.5 lg:grid-cols-3">
                                {light.map((j, i) =>
                                    j ? (
                                        <RunningJob
                                            key={j.ticket}
                                            job={j}
                                            snap={snap}
                                            p50s={p50s}
                                            compact
                                            selected={selected === j.ticket}
                                            onSelect={() => toggle(j.ticket)}
                                        />
                                    ) : (
                                        <FreeSlot key={`l${i}`} compact label="free" />
                                    ),
                                )}
                            </div>
                        </div>
                    </div>
                </div>

                {sel && (
                    <div className="relative z-10 mt-4 rounded-xl border border-white/10 bg-[#020618]/85 p-3">
                        <div className="mb-2 flex items-center gap-2">
                            <span className="rounded-full bg-white/6 px-1.5 py-px text-[10.5px] font-semibold text-(--ciq-muted)">{sel.class}</span>
                            <span className="truncate text-[13.5px] font-medium text-(--ciq-text)">{sel.label}</span>
                            <button type="button" onClick={() => setSelected(null)} className="ml-auto text-[12px] text-(--ciq-faint) hover:text-(--ciq-text)">
                                Close
                            </button>
                        </div>
                        <Detail job={sel} snap={snap} p50s={p50s} />
                    </div>
                )}
            </section>

            {/* ── Pressure + budget ─────────────────────────────────────── */}
            <div className={`grid gap-4 ${snap.budget ? 'xl:grid-cols-[minmax(0,1.35fr)_minmax(0,1fr)]' : ''}`}>
                <Panel icon={<Activity className="size-4" />} title="Pressure" aside="the line is where the gate holds new heavy jobs · last 10 min">
                    <div className="divide-y divide-white/5">
                        {sigs.map((s) => (
                            <Gauge key={s.key} s={s} series={signalSeries(samples, snap.config.gate, s.key)} />
                        ))}
                    </div>
                    <p className="mt-2 text-[11.5px] text-(--ciq-faint)">
                        One heavy job is always admitted when none is running, so a busy machine still makes progress.
                    </p>
                </Panel>
                <BudgetPanel snap={snap} samples={samples} />
            </div>

            <div
                className={`grid gap-4 ${costs.length ? 'lg:grid-cols-2 2xl:grid-cols-[minmax(0,0.8fr)_minmax(0,1fr)_minmax(0,1.2fr)]' : 'lg:grid-cols-[minmax(0,1fr)_minmax(0,1.4fr)]'}`}
            >
                <Panel icon={<Lock className="size-4" />} title="Locks" aside={locks.length ? `${locks.length} held` : undefined}>
                    {locks.length === 0 ? (
                        <p className="py-2 text-[12.5px] text-(--ciq-faint)">No shared resource is held right now.</p>
                    ) : (
                        <ul className="flex flex-col gap-2.5">
                            {locks.map((l) => (
                                <li key={`${l.lock}-${l.holder.ticket}`} className="ciq-lock-held rounded-xl border border-transparent px-3 py-2 text-[12.5px]">
                                    <div className="flex items-center gap-1.5 truncate font-mono text-[11.5px]" style={{ color: HEAT[3] }}>
                                        <Lock className="size-3 shrink-0" />
                                        {shortLock(l.lock)}
                                    </div>
                                    <div className="mt-0.5 text-(--ciq-text)">
                                        held by <span className="tabular-nums">#{l.holder.ticket}</span> {l.holder.label}
                                    </div>
                                    {l.waiters.length > 0 && (
                                        <div className="mt-0.5 text-(--ciq-muted)">
                                            {l.waiters.length} waiting for it: {l.waiters.map((w) => `#${w.ticket}`).join(', ')}
                                        </div>
                                    )}
                                </li>
                            ))}
                        </ul>
                    )}
                </Panel>
                {costs.length > 0 && (
                    <Panel icon={<ScatterChart className="size-4" />} title="What each check costs" aside="typical run time × peak memory · dot = cores busy">
                        <CostScatter costs={costs} />
                    </Panel>
                )}
                <Panel icon={<History className="size-4" />} title="Recent" className={costs.length ? 'lg:col-span-2 2xl:col-span-1' : ''}>
                    {recent.length === 0 ? (
                        <p className="py-2 text-[12.5px] text-(--ciq-faint)">No finished jobs yet.</p>
                    ) : (
                        <ul className="divide-y divide-white/5">
                            {recent.map((h, i) => (
                                <HistoryRow key={`${h.ticket}-${h.queued_at_ms}`} h={h} live={i < 3 && !all.some((j) => j.ticket === h.ticket)} />
                            ))}
                        </ul>
                    )}
                </Panel>
            </div>
        </main>
    );
}

/** Where the replay is in the night: calm → storm → drain, as a strip. */
function DemoStrip({ phase: name, t }: { phase: string; t: number }) {
    const x = ((t % PERIOD_S) + PERIOD_S) % PERIOD_S;
    const segs = [
        { name: 'calm', from: 0, to: 60 },
        { name: 'storm', from: 60, to: 240 },
        { name: 'drain', from: 240, to: PERIOD_S },
    ];
    return (
        <span className="flex flex-wrap items-center gap-2">
            <span>Replaying 2026-09-26: 35 agents, one 12-core Mac, load 1,022.</span>
            <span className="relative flex h-1.5 w-40 overflow-hidden rounded-full bg-white/8" aria-label={`replay at ${name}`}>
                {segs.map((s) => (
                    <span
                        key={s.name}
                        className="h-full"
                        style={{
                            width: `${((s.to - s.from) / PERIOD_S) * 100}%`,
                            background: s.name === 'storm' ? HEAT[5] : s.name === 'calm' ? HEAT[0] : HEAT[1],
                            opacity: s.name === name ? 0.9 : 0.3,
                        }}
                    />
                ))}
                <span className="absolute inset-y-[-2px] w-[2px] bg-white" style={{ left: `${(x / PERIOD_S) * 100}%` }} />
            </span>
            <span className="text-(--ciq-text)">{name}</span>
        </span>
    );
}

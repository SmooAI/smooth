//! The Queue tab — `th ci-queue`, live (SMOODEV-3371).
//!
//! One picture of the machine: waiting jobs line up on the left, pass through
//! the gate, and run in the heavy and light slot lanes on the right. The gate
//! is the one bold element. It glows with the machine's heat (Aurora spectrum,
//! teal → gold → coral) and closes when the pressure gate is holding heavy
//! jobs. The same gate line runs down through the pressure gauges, because
//! every gauge draws its threshold at the same x.
//!
//! Data: `GET /api/ci-queue/status` on the daemon, polled each second while
//! the tab is visible; `?demo` swaps in the seeded simulator from ci-queue.ts.
//! Jobs move between the queue, the lanes and the history via the View
//! Transitions API, keyed by ticket, so an admission reads as the job itself
//! sliding through the gate. Reduced motion gets plain state swaps.

import { Activity, ChevronRight, History, Lock, Pause, Play, RefreshCw } from 'lucide-react';
import { useCallback, useEffect, useMemo, useRef, useState, type CSSProperties, type ReactNode } from 'react';
import { flushSync } from 'react-dom';

import {
    appendSamples,
    DemoQueue,
    diff,
    duration,
    gaugeFill,
    GATE_X,
    HEAT,
    heatOf,
    labelP50s,
    lanes,
    lockHolders,
    machineHeat,
    progress,
    SCHEMA,
    shortLock,
    signals,
    signalSeries,
    waitReason,
    worktreeOf,
    type HistoryEntry,
    type JobInfo,
    type Sample,
    type Signal,
    type Snapshot,
    type StatusResponse,
} from './ci-queue';
import { resolveTarget } from './operator';

const WINDOW_MS = 10 * 60_000;

function reducedMotion(): boolean {
    return typeof window !== 'undefined' && window.matchMedia('(prefers-reduced-motion: reduce)').matches;
}

/** Apply a state change inside a view transition when one would show a job
 * moving, so the element keyed by its ticket animates between places. */
function withTransition(moved: boolean, apply: () => void): void {
    const doc = document as Document & { startViewTransition?: (cb: () => void) => unknown };
    if (moved && doc.startViewTransition && !reducedMotion() && document.visibilityState === 'visible') {
        doc.startViewTransition(() => flushSync(apply));
    } else {
        apply();
    }
}

interface Feed {
    snap: Snapshot | null;
    samples: Sample[];
    error: string | null;
    demo: boolean;
    paused: boolean;
    setPaused: (p: boolean) => void;
    refresh: () => void;
}

/** Poll the daemon (or step the demo) once a second while the page is visible. */
function useCiQueue(): Feed {
    const demo = useMemo(() => new URLSearchParams(window.location.search).has('demo'), []);
    const [snap, setSnap] = useState<Snapshot | null>(null);
    const [samples, setSamples] = useState<Sample[]>([]);
    const [error, setError] = useState<string | null>(null);
    const [paused, setPaused] = useState(false);
    const prev = useRef<Snapshot | null>(null);
    const sim = useRef<DemoQueue | null>(null);
    const lastSample = useRef(0);

    const accept = useCallback((next: Snapshot, add: Sample[]) => {
        const moved = diff(prev.current, next).some((e) => e.kind !== 'queued');
        prev.current = next;
        withTransition(moved, () => {
            setSnap(next);
            setSamples((s) => appendSamples(s, add, WINDOW_MS));
            setError(null);
        });
    }, []);

    const tick = useCallback(async () => {
        if (demo) {
            if (!sim.current) {
                // Warm the simulator up so the sparklines open with ten minutes behind them.
                sim.current = new DemoQueue();
                const warm: Sample[] = [];
                for (let i = 0; i < 600; i++) {
                    sim.current.step();
                    if (i % 5 === 0) warm.push(sim.current.sample());
                }
                accept(sim.current.snapshot(), warm);
                return;
            }
            const next = sim.current.step();
            accept(next, [sim.current.sample()]);
            return;
        }
        const { http, token } = resolveTarget();
        try {
            const r = await fetch(`${http}/api/ci-queue/status?since_ms=${lastSample.current}`, {
                headers: token ? { authorization: `Bearer ${token}` } : {},
            });
            if (!r.ok) {
                setError(
                    r.status === 404
                        ? 'This Big Smooth is older than the Queue tab. Update it with `pnpm install:th` or the menu bar.'
                        : `The daemon answered ${r.status}.`,
                );
                return;
            }
            const body = (await r.json()) as StatusResponse;
            if (body.samples.length) lastSample.current = body.samples[body.samples.length - 1].t_ms;
            if (body.error || !body.snapshot) {
                setError(body.error ?? 'The daemon returned no queue snapshot.');
                return;
            }
            accept(body.snapshot, body.samples);
        } catch {
            setError('Big Smooth is not answering. Start it with `th up`.');
        }
    }, [accept, demo]);

    useEffect(() => {
        if (paused) return;
        void tick();
        const id = window.setInterval(() => {
            if (document.visibilityState === 'visible') void tick();
        }, 1000);
        return () => window.clearInterval(id);
    }, [tick, paused]);

    return { snap, samples, error, demo, paused, setPaused, refresh: () => void tick() };
}

// ── Pieces ──────────────────────────────────────────────────────────────────

const transitionName = (ticket: number): CSSProperties => ({ viewTransitionName: `ciq-${ticket}` }) as CSSProperties;

function ClassChip({ cls }: { cls: 'heavy' | 'light' }) {
    return (
        <span
            className={`rounded-full px-1.5 py-px text-[10.5px] font-semibold ${cls === 'heavy' ? 'bg-(--ciq-heat)/15 text-(--ciq-heat)' : 'bg-white/6 text-(--ciq-muted)'}`}
        >
            {cls}
        </span>
    );
}

/** The kind of thing holding a waiter back, for its colour: a lock is a
 * neighbour (gold), pressure is the machine (the hot end), slots are just a line. */
function reasonTone(reason: string): string {
    if (/held by|cargo:|lock/i.test(reason)) return HEAT[3];
    if (/busy|ahead|next in line/i.test(reason)) return 'var(--ciq-muted)';
    return HEAT[5];
}

function Waiter({ job, snap, selected, onSelect }: { job: JobInfo; snap: Snapshot; selected: boolean; onSelect: () => void }) {
    const reason = waitReason(job, snap);
    return (
        <li style={transitionName(job.ticket)}>
            <button
                type="button"
                onClick={onSelect}
                aria-pressed={selected}
                className={`ciq-ticket group w-full rounded-xl border px-3 py-2 text-left transition ${selected ? 'border-(--ciq-heat)/60 bg-white/5' : 'border-(--ciq-border) hover:border-white/15'}`}
            >
                <div className="flex items-baseline gap-2">
                    <span className="font-mono text-[11px] text-(--ciq-faint) tabular-nums">#{job.ticket}</span>
                    <span className="min-w-0 flex-1 truncate text-[13px] text-(--ciq-text)">{job.label}</span>
                    <span className="text-[11px] text-(--ciq-muted) tabular-nums">{duration(snap.now_ms - job.queued_at_ms)}</span>
                </div>
                <div className="mt-0.5 flex items-center gap-1.5 text-[11.5px]" style={{ color: reasonTone(reason) }}>
                    {(job.locks?.length ?? 0) > 0 && <Lock className="size-3 shrink-0" aria-label="needs a lock" />}
                    <span className="truncate" title={reason}>
                        {reason}
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
    return (
        <button
            type="button"
            onClick={onSelect}
            aria-pressed={selected}
            style={transitionName(job.ticket)}
            className={`ciq-capsule relative block w-full overflow-hidden rounded-xl border text-left ${selected ? 'border-(--ciq-heat)/70' : 'border-white/10'} bg-white/[0.035]`}
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
                {(job.locks?.length ?? 0) > 0 && <Lock className="size-3.5 shrink-0 text-(--ciq-muted)" aria-label="holds a lock" />}
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
        <div className="ciq-gate-wrap relative flex items-stretch justify-center md:w-10" aria-hidden>
            <div className={`ciq-gate ${holding ? 'ciq-gate--held' : ''}`} />
            {!holding && (
                <div className="ciq-flow pointer-events-none absolute inset-0 hidden md:block">
                    <ChevronRight className="ciq-flow-chev absolute left-1/2 size-4 -translate-x-1/2" />
                </div>
            )}
        </div>
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
        <div className="grid grid-cols-[minmax(0,1fr)] items-center gap-x-4 gap-y-1 pt-2 pb-5 sm:grid-cols-[10.5rem_minmax(0,1fr)_120px] sm:pb-4">
            <div className="min-w-0">
                <div className="flex items-baseline justify-between gap-2 sm:block">
                    <div className="text-[13px] text-(--ciq-text)">{s.name}</div>
                    <div className="text-[12px] tabular-nums" style={{ color: heat }}>
                        {s.value}
                        {s.off && <span className="text-(--ciq-faint)"> · off</span>}
                    </div>
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

function Panel({ icon, title, aside, children }: { icon: ReactNode; title: string; aside?: ReactNode; children: ReactNode }) {
    return (
        <section className="rounded-[18px] border border-(--ciq-border) bg-(--ciq-card) p-4">
            <h2 className="mb-2 flex items-center gap-2 text-[13px] font-semibold text-(--ciq-text)">
                <span className="text-(--ciq-heat)">{icon}</span>
                {title}
                {aside && <span className="ml-auto text-[11.5px] font-normal text-(--ciq-faint)">{aside}</span>}
            </h2>
            {children}
        </section>
    );
}

function HistoryRow({ h, live }: { h: HistoryEntry; live: boolean }) {
    const ok = h.outcome === 'exit' && h.exit === 0;
    const tone = ok ? '#34d399' : h.outcome === 'wait-timeout' ? HEAT[3] : HEAT[5];
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

function Detail({ job, snap, p50s }: { job: JobInfo; snap: Snapshot; p50s: Map<string, number> }) {
    const running = job.admitted_at_ms != null;
    const rows: Array<[string, ReactNode]> = [
        ['Ticket', `#${job.ticket} · ${job.class}${running ? ` slot ${job.slot ?? '?'}` : ''}`],
        [
            'Worktree',
            <span key="cwd" className="break-all">
                {job.cwd}
            </span>,
        ],
        ['Process', `th pid ${job.pid}${job.child_pid ? ` · job pid ${job.child_pid}` : ''}`],
        ['Queued', new Date(job.queued_at_ms).toLocaleTimeString()],
        running
            ? [
                  'Running',
                  `${duration(snap.now_ms - (job.admitted_at_ms ?? 0))} (usually ${p50s.get(job.label) ? duration(p50s.get(job.label) ?? 0) : 'unknown — first run here'})`,
              ]
            : ['Waiting on', waitReason(job, snap)],
    ];
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

// ── Page ────────────────────────────────────────────────────────────────────

export default function CiQueuePage() {
    const { snap, samples, error, demo, paused, setPaused, refresh } = useCiQueue();
    const [selected, setSelected] = useState<number | null>(null);
    const p50s = useMemo(() => labelP50s(snap?.history ?? []), [snap?.history]);

    const heat = snap ? machineHeat(snap) : 0;
    const style = { '--ciq-heat': HEAT[heat] } as CSSProperties;

    if (!snap) {
        return (
            <main className="ciq flex min-h-0 flex-1 flex-col items-center justify-center py-6" style={style}>
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
    const recent = [...snap.history].reverse().slice(0, 8);
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
        <main
            className="ciq flex min-h-0 flex-1 flex-col gap-4 overflow-y-auto pt-[calc(env(safe-area-inset-top)+4rem)] pb-6 sm:pt-6 [&>*]:shrink-0"
            style={style}
        >
            {/* ── The machine ───────────────────────────────────────────── */}
            <section className="ciq-chamber relative overflow-hidden rounded-[18px] border border-white/10 p-4 sm:p-5">
                <div className="ciq-ambient" aria-hidden />
                <header className="relative flex flex-wrap items-start gap-x-6 gap-y-2">
                    <div className="min-w-0 flex-1">
                        <h1 className="font-display text-[22px] leading-tight font-bold tracking-[-0.02em] text-balance text-(--ciq-text) sm:text-[26px]">
                            {verdict}
                        </h1>
                        <p className="mt-1 text-[12.5px] text-(--ciq-muted)">Heavy checks on this machine take a slot first{demo ? '. Demo data.' : '.'}</p>
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
                {error && <p className="relative mt-3 rounded-xl bg-white/5 px-3 py-2 text-[12.5px] text-(--ciq-muted)">{error} Showing the last reading.</p>}
                {snap.schema != null && snap.schema > SCHEMA && (
                    <p className="relative mt-3 text-[12px]" style={{ color: HEAT[3] }}>
                        This th reports a newer queue format ({snap.schema}); some fields may be missing here.
                    </p>
                )}

                <div className="relative mt-5 grid gap-4 md:grid-cols-[minmax(0,19rem)_auto_minmax(0,1fr)] md:gap-3">
                    <div>
                        <div className="mb-2 text-[12px] text-(--ciq-muted)">In line, first come first served</div>
                        {snap.waiting.length === 0 ? (
                            <p className="rounded-xl border border-dashed border-white/8 px-3 py-4 text-[12.5px] text-(--ciq-faint)">Nobody waiting.</p>
                        ) : (
                            <ol className="ciq-line flex max-h-[23rem] flex-col gap-1.5 overflow-y-auto pr-1">
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
                    <div className="relative mt-4 rounded-xl border border-white/10 bg-black/20 p-3">
                        <div className="mb-2 flex items-center gap-2">
                            <ClassChip cls={sel.class} />
                            <span className="truncate text-[13.5px] font-medium text-(--ciq-text)">{sel.label}</span>
                            <button type="button" onClick={() => setSelected(null)} className="ml-auto text-[12px] text-(--ciq-faint) hover:text-(--ciq-text)">
                                Close
                            </button>
                        </div>
                        <Detail job={sel} snap={snap} p50s={p50s} />
                    </div>
                )}
            </section>

            {/* ── Pressure ──────────────────────────────────────────────── */}
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

            <div className="grid gap-4 lg:grid-cols-[minmax(0,1fr)_minmax(0,1.4fr)]">
                <Panel icon={<Lock className="size-4" />} title="Locks" aside={locks.length ? `${locks.length} held` : undefined}>
                    {locks.length === 0 ? (
                        <p className="py-2 text-[12.5px] text-(--ciq-faint)">No shared resource is held right now.</p>
                    ) : (
                        <ul className="flex flex-col gap-2.5">
                            {locks.map((l) => (
                                <li key={`${l.lock}-${l.holder.ticket}`} className="text-[12.5px]">
                                    <div className="truncate font-mono text-[11.5px] text-(--ciq-muted)">{shortLock(l.lock)}</div>
                                    <div className="mt-0.5 text-(--ciq-text)">
                                        held by <span className="tabular-nums">#{l.holder.ticket}</span> {l.holder.label}
                                    </div>
                                    {l.waiters.length > 0 && (
                                        <div className="mt-0.5" style={{ color: HEAT[3] }}>
                                            {l.waiters.length} waiting for it: {l.waiters.map((w) => `#${w.ticket}`).join(', ')}
                                        </div>
                                    )}
                                </li>
                            ))}
                        </ul>
                    )}
                </Panel>
                <Panel
                    icon={<History className="size-4" />}
                    title="Recent"
                    aside={
                        <button type="button" onClick={refresh} className="flex items-center gap-1 hover:text-(--ciq-text)">
                            <RefreshCw className="size-3" /> Refresh
                        </button>
                    }
                >
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

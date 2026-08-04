//! The Stats tab — usage + spend. Spend is the authoritative per-call cost the
//! LiteLLM gateway reports (`x-litellm-response-cost-*` → the engine's
//! `gateway_cost_usd`), persisted per-turn to `~/.smooth/usage.jsonl` via
//! `/api/usage` and aggregated by `GET /api/stats`; activity comes from the operator db.
//!
//! Presence styling: teal carries proportion (the only "live" signal here);
//! everything else is panel + muted-foreground. No amber — nothing here needs you.

import { BarChart3, MessageSquare, RefreshCw, Wallet } from 'lucide-react';
import { useCallback, useEffect, useState } from 'react';

import { fetchStats, type Stats, type StatsBucket } from './operator';

function usd(n: number): string {
    if (!Number.isFinite(n) || n === 0) return '$0.00';
    return n < 0.01 ? `$${n.toFixed(4)}` : `$${n.toFixed(2)}`;
}

function compact(n: number): string {
    if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
    if (n >= 1_000) return `${(n / 1_000).toFixed(1)}k`;
    return `${n}`;
}

function Tile({ label, value, sub }: { label: string; value: string; sub?: string }) {
    return (
        <div className="rounded-2xl border border-border bg-panel/90 p-4 backdrop-blur">
            <div className="text-xs uppercase tracking-wide text-(--color-muted-foreground)">{label}</div>
            <div className="mt-1 font-display text-2xl text-foreground">{value}</div>
            {sub && <div className="mt-0.5 text-xs text-(--color-muted-foreground)">{sub}</div>}
        </div>
    );
}

/** A labeled proportion bar — teal fill, width = value / max. */
function BarRow({ label, valueLabel, frac }: { label: string; valueLabel: string; frac: number }) {
    return (
        <div className="flex items-center gap-3">
            <div className="w-28 shrink-0 truncate font-mono text-xs text-foreground/80" title={label}>
                {label}
            </div>
            <div className="h-2 flex-1 overflow-hidden rounded-full bg-panel-2">
                <div className="h-full rounded-full bg-(--color-th-teal)" style={{ width: `${Math.max(2, Math.round(frac * 100))}%` }} />
            </div>
            <div className="w-20 shrink-0 text-right font-mono text-xs text-(--color-muted-foreground)">{valueLabel}</div>
        </div>
    );
}

function Section({ icon, title, children }: { icon: React.ReactNode; title: string; children: React.ReactNode }) {
    return (
        <section className="mt-6">
            <h2 className="mb-3 flex items-center gap-2 text-sm font-semibold text-foreground/90">
                <span className="text-(--color-th-teal)">{icon}</span>
                {title}
            </h2>
            {children}
        </section>
    );
}

export default function StatsPage() {
    const [stats, setStats] = useState<Stats | null>(null);
    const [loading, setLoading] = useState(true);

    const load = useCallback(() => {
        setLoading(true);
        void fetchStats().then((s) => {
            setStats(s);
            setLoading(false);
        });
    }, []);

    useEffect(load, [load]);

    const spend = stats?.spend;
    const activity = stats?.activity;
    const nothingYet = !loading && (!spend || spend.turns === 0) && (!activity || activity.conversations === 0);
    const maxModelUsd = Math.max(1e-9, ...(spend?.by_model ?? []).map((b) => b.usd));
    const maxDayUsd = Math.max(1e-9, ...(spend?.by_day ?? []).map((b) => b.usd));

    return (
        <main className="flex min-h-0 flex-1 flex-col overflow-y-auto py-6">
            <div className="flex items-center justify-between">
                <h1 className="font-display text-xl text-foreground">Usage &amp; Spend</h1>
                <button
                    type="button"
                    onClick={load}
                    className="flex items-center gap-1.5 rounded-full border border-border px-3 py-1.5 text-xs text-(--color-muted-foreground) hover:bg-panel-2"
                >
                    <RefreshCw className={`size-3.5 ${loading ? 'animate-spin' : ''}`} /> Refresh
                </button>
            </div>

            {loading && !stats ? (
                <div className="mt-10 text-center text-sm text-(--color-muted-foreground)">Loading…</div>
            ) : nothingYet ? (
                <div className="mt-10 rounded-2xl border border-border bg-panel/60 p-6 text-center text-sm text-(--color-muted-foreground)">
                    No usage yet. Spend and activity appear here as you chat with Big Smooth.
                </div>
            ) : (
                <>
                    <Section icon={<Wallet className="size-4" />} title="Spend">
                        <div className="grid grid-cols-2 gap-3 sm:grid-cols-4">
                            <Tile label="Total spend" value={usd(spend?.total_usd ?? 0)} sub={`${spend?.turns ?? 0} turns`} />
                            <Tile label="Input tokens" value={compact(spend?.prompt_tokens ?? 0)} />
                            <Tile label="Output tokens" value={compact(spend?.completion_tokens ?? 0)} />
                            <Tile label="Avg / turn" value={usd(spend && spend.turns > 0 ? spend.total_usd / spend.turns : 0)} />
                        </div>

                        {(spend?.by_model.length ?? 0) > 0 && (
                            <div className="mt-4 space-y-2 rounded-2xl border border-border bg-panel/60 p-4">
                                <div className="mb-1 text-xs uppercase tracking-wide text-(--color-muted-foreground)">By model</div>
                                {spend?.by_model.map((b: StatsBucket) => (
                                    <BarRow key={b.key} label={b.key} valueLabel={usd(b.usd)} frac={b.usd / maxModelUsd} />
                                ))}
                            </div>
                        )}

                        {(spend?.by_day.length ?? 0) > 1 && (
                            <div className="mt-4 space-y-2 rounded-2xl border border-border bg-panel/60 p-4">
                                <div className="mb-1 text-xs uppercase tracking-wide text-(--color-muted-foreground)">By day</div>
                                {spend?.by_day.map((b: StatsBucket) => (
                                    <BarRow key={b.key} label={b.key} valueLabel={usd(b.usd)} frac={b.usd / maxDayUsd} />
                                ))}
                            </div>
                        )}
                    </Section>

                    <Section icon={<MessageSquare className="size-4" />} title="Activity">
                        <div className="grid grid-cols-2 gap-3 sm:grid-cols-4">
                            <Tile label="Conversations" value={`${activity?.conversations ?? 0}`} />
                            <Tile
                                label="Sessions"
                                value={`${activity?.sessions ?? 0}`}
                                sub={`${activity?.active_sessions ?? 0} active · ${activity?.ended_sessions ?? 0} ended`}
                            />
                            <Tile label="Messages" value={`${activity?.messages ?? 0}`} sub={`${activity?.inbound ?? 0} in · ${activity?.outbound ?? 0} out`} />
                            <Tile
                                label="Last active"
                                value={activity?.last_activity ? new Date(activity.last_activity).toLocaleDateString() : '—'}
                                sub={activity?.last_activity ? new Date(activity.last_activity).toLocaleTimeString() : undefined}
                            />
                        </div>
                    </Section>

                    <p className="mt-6 flex items-center gap-1.5 text-xs text-(--color-muted-foreground)">
                        <BarChart3 className="size-3.5" />
                        Spend is the actual per-call cost the LiteLLM gateway reports for each turn.
                    </p>
                </>
            )}
        </main>
    );
}

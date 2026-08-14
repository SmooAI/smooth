//! The searchable model selector — replaces the old fixed preset chips
//! (th-3a5d22). Type to filter every benchmarked model; each row carries its
//! capability badges, pass rate, and $/pass from `docs/model-scores.json`, plus
//! the live per-token cost glyph. The composer wraps it in a popover; Settings
//! embeds the same picker inline. Presence styling: teal marks the active model.

import { Check, ChevronDown, Search } from 'lucide-react';
import { useEffect, useMemo, useRef, useState } from 'react';

import { BENCH_DATE, BENCH_SCENARIOS, BENCH_SUITE, BENCH_TRIALS, MODEL_ROWS, costBadge, modeById, type ModelCosts, type ModelRow } from './modes';

/** $/pass, or an explicit "unknown" — a null cost is NEVER shown as $0/free. */
function fmtPerPass(n: number | null): string {
    if (n === null) return 'cost unknown';
    return `$${(n < 0.001 ? n.toFixed(5) : n.toFixed(4)).replace(/0+$/, '').replace(/\.$/, '')}/pass`;
}

/** The live 💚/💛/🧡/❤️ blended-rate glyph for a model, or null when the
 * /admin/model-costs endpoint hasn't priced it. */
function costGlyph(model: string, costs: ModelCosts): string | null {
    const c = costs[model];
    return c ? costBadge(c.inputCostPerToken, c.outputCostPerToken) : null;
}

/** One row's inner layout (shared by the popover and Settings). */
function RowBody({ row, active, glyph }: { row: ModelRow; active: boolean; glyph: string | null }) {
    return (
        <>
            <span aria-hidden className="flex w-12 shrink-0 items-center justify-start gap-0.5">
                {row.badges.map((b) => (
                    <span key={b.emoji} title={b.label} aria-label={b.label}>
                        {b.emoji}
                    </span>
                ))}
            </span>
            <span className={`min-w-0 flex-1 truncate font-mono text-xs ${active ? 'text-(--color-th-teal)' : ''}`}>{row.model}</span>
            <span className="shrink-0 tabular-nums text-xs text-(--color-muted-foreground)">{row.passRatePct.toFixed(1)}%</span>
            <span className="flex shrink-0 items-center gap-1 tabular-nums text-xs text-(--color-muted-foreground)">
                {glyph && <span aria-hidden>{glyph}</span>}
                {fmtPerPass(row.costPerPassUsd)}
            </span>
            <span className="w-4 shrink-0">{active && <Check className="size-4 text-(--color-th-teal)" />}</span>
        </>
    );
}

/** The search box + filtered model list. Used inline in Settings and inside the
 * composer's popover. */
export function ModelPicker({
    current,
    onPick,
    costs,
    autoFocus,
}: {
    current: string;
    onPick: (model: string) => void;
    costs: ModelCosts;
    autoFocus?: boolean;
}) {
    const [q, setQ] = useState('');
    const rows = useMemo(() => {
        const needle = q.trim().toLowerCase();
        return needle ? MODEL_ROWS.filter((r) => r.model.toLowerCase().includes(needle)) : MODEL_ROWS;
    }, [q]);

    return (
        <div className="flex min-h-0 flex-col">
            <div className="flex items-center gap-2 border-b border-border/60 px-3 py-2">
                <Search className="size-4 shrink-0 text-(--color-muted-foreground)" />
                <input
                    value={q}
                    onChange={(e) => setQ(e.target.value)}
                    placeholder="Search models…"
                    aria-label="Search models"
                    autoFocus={autoFocus}
                    className="w-full bg-transparent text-sm outline-none placeholder:text-(--color-muted-foreground)"
                />
            </div>
            <ul className="max-h-72 overflow-y-auto py-1">
                {rows.map((r) => (
                    <li key={r.model}>
                        <button
                            type="button"
                            onClick={() => onPick(r.model)}
                            className={`flex w-full items-center gap-2 px-3 py-1.5 text-left transition hover:bg-panel-2 ${r.model === current ? 'bg-(--color-th-teal)/10' : ''}`}
                        >
                            <RowBody row={r} active={r.model === current} glyph={costGlyph(r.model, costs)} />
                        </button>
                    </li>
                ))}
                {rows.length === 0 && <li className="px-3 py-2 text-sm text-(--color-muted-foreground)">No models match “{q.trim()}”.</li>}
            </ul>
            <div className="border-t border-border/60 px-3 py-1.5 text-[0.7rem] text-(--color-muted-foreground)">
                benchmarked {BENCH_DATE} · {BENCH_SUITE} · {BENCH_TRIALS} trials × {BENCH_SCENARIOS} scenarios
            </div>
        </div>
    );
}

/** The composer's model button: shows the active model + primary badge; opens the
 * picker in a popover above it. Closes on pick, outside click, or Esc. */
export function ModelSelectorButton({ current, onPick, costs }: { current: string; onPick: (model: string) => void; costs: ModelCosts }) {
    const [open, setOpen] = useState(false);
    const ref = useRef<HTMLDivElement>(null);
    const active = modeById(current);

    useEffect(() => {
        if (!open) return;
        const onDoc = (e: MouseEvent) => {
            if (ref.current && !ref.current.contains(e.target as Node)) setOpen(false);
        };
        const onKey = (e: KeyboardEvent) => {
            if (e.key === 'Escape') setOpen(false);
        };
        document.addEventListener('mousedown', onDoc);
        document.addEventListener('keydown', onKey);
        return () => {
            document.removeEventListener('mousedown', onDoc);
            document.removeEventListener('keydown', onKey);
        };
    }, [open]);

    return (
        <div ref={ref} className="relative">
            <button
                type="button"
                onClick={() => setOpen((o) => !o)}
                aria-haspopup="listbox"
                aria-expanded={open}
                title="Choose the model for this session"
                className="flex items-center gap-1.5 rounded-xl border border-border bg-panel/60 px-2.5 py-1 text-xs text-foreground/80 transition hover:bg-panel-2"
            >
                <span aria-hidden>{active.emoji}</span>
                <span className="max-w-40 truncate font-mono">{active.model}</span>
                <ChevronDown className={`size-3.5 shrink-0 text-(--color-muted-foreground) transition ${open ? 'rotate-180' : ''}`} />
            </button>
            {open && (
                <div className="absolute bottom-full left-0 z-40 mb-2 w-80 max-w-[calc(100vw-2rem)] overflow-hidden rounded-2xl border border-border bg-panel/95 shadow-xl backdrop-blur">
                    <ModelPicker
                        current={current}
                        costs={costs}
                        autoFocus
                        onPick={(m) => {
                            onPick(m);
                            setOpen(false);
                        }}
                    />
                </div>
            )}
        </div>
    );
}

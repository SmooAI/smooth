//! The searchable model selector — replaces the old fixed preset chips
//! (th-3a5d22). Type to filter every offered model. Each row puts the model's
//! NAME first on its own line (SMOODEV-4174: badges and cost text used to squeeze
//! every name down to "g."), with score, $/pass, earned badges and the live token
//! price on a quiet second line. Main models come first. The composer wraps it in
//! a popover; Settings embeds the same picker inline. Presence styling: teal
//! marks the active model.

import { Check, ChevronDown, Search } from 'lucide-react';
import { useEffect, useMemo, useRef, useState } from 'react';

import {
    BENCH_META,
    MODEL_ROWS,
    benchFooter,
    fetchModelCatalog,
    isMainModel,
    modeById,
    modelLabel,
    priceText,
    rowMeta,
    type BenchMeta,
    type ModelCosts,
    type ModelRow,
} from './modes';

/** The picker rows + bench metadata: the bundled catalog immediately (instant
 * render, offline fallback), replaced by the daemon's live `/api/model-catalog`
 * once it loads — the single source of truth so a bench refresh shows here
 * without a rebuild (th-1d8007). */
function useModelCatalog(): { rows: ModelRow[]; meta: BenchMeta } {
    const [catalog, setCatalog] = useState<{ rows: ModelRow[]; meta: BenchMeta }>({ rows: MODEL_ROWS, meta: BENCH_META });
    useEffect(() => {
        let alive = true;
        void fetchModelCatalog().then((live) => {
            if (alive && live) setCatalog(live);
        });
        return () => {
            alive = false;
        };
    }, []);
    return catalog;
}

/** One row's inner layout (shared by the popover and Settings): the name owns
 * line one; everything else is muted metadata on line two. */
function RowBody({ row, active, price }: { row: ModelRow; active: boolean; price: string | null }) {
    const label = modelLabel(row.model);
    return (
        <>
            <span className="flex min-w-0 flex-1 flex-col gap-0.5">
                <span className={`break-words text-sm leading-snug ${active ? 'font-medium text-(--color-th-teal)' : 'text-foreground'}`}>{label}</span>
                <span className="flex flex-wrap items-center gap-x-1.5 gap-y-0.5 text-[0.7rem] leading-tight text-(--color-muted-foreground)">
                    {label !== row.model && <span className="font-mono">{row.model}</span>}
                    <span className="tabular-nums">{rowMeta(row)}</span>
                    {row.badges.map((b) => (
                        <span key={b.emoji} title={b.label} aria-label={b.label}>
                            {b.emoji}
                        </span>
                    ))}
                    {row.premium && (
                        <span title="Premium tier: highest quality, higher cost" className="rounded border border-border px-1 uppercase tracking-wide">
                            premium
                        </span>
                    )}
                    {price && (
                        <span className="tabular-nums" title="Live blended token price (average of input and output, per million tokens)">
                            {price}
                        </span>
                    )}
                </span>
            </span>
            <span className="w-4 shrink-0 self-center">{active && <Check className="size-4 text-(--color-th-teal)" />}</span>
        </>
    );
}

function GroupLabel({ children }: { children: string }) {
    return <li className="px-3 pt-2 pb-1 text-[0.65rem] font-medium uppercase tracking-wider text-(--color-muted-foreground)">{children}</li>;
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
    const { rows: allRows, meta } = useModelCatalog();
    const rows = useMemo(() => {
        const needle = q.trim().toLowerCase();
        return needle ? allRows.filter((r) => r.model.toLowerCase().includes(needle) || modelLabel(r.model).toLowerCase().includes(needle)) : allRows;
    }, [q, allRows]);
    const main = rows.filter((r) => isMainModel(r.model));
    const rest = rows.filter((r) => !isMainModel(r.model));
    const grouped = main.length > 0 && rest.length > 0;

    const item = (r: ModelRow) => (
        <li key={r.model}>
            <button
                type="button"
                onClick={() => onPick(r.model)}
                title={r.model}
                className={`flex w-full items-start gap-2 px-3 py-1.5 text-left transition hover:bg-panel-2 ${r.model === current ? 'bg-(--color-th-teal)/10' : ''}`}
            >
                <RowBody row={r} active={r.model === current} price={priceText(costs[r.model])} />
            </button>
        </li>
    );

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
            <ul className="max-h-80 overflow-y-auto py-1">
                {grouped && <GroupLabel>Main models</GroupLabel>}
                {main.map(item)}
                {grouped && <GroupLabel>More models</GroupLabel>}
                {rest.map(item)}
                {rows.length === 0 && <li className="px-3 py-2 text-sm text-(--color-muted-foreground)">No models match “{q.trim()}”.</li>}
            </ul>
            <div className="border-t border-border/60 px-3 py-1.5 text-[0.7rem] leading-snug text-(--color-muted-foreground)">{benchFooter(meta, allRows)}</div>
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
                <span className="max-w-44 truncate" title={active.model}>
                    {active.label}
                </span>
                <ChevronDown className={`size-3.5 shrink-0 text-(--color-muted-foreground) transition ${open ? 'rotate-180' : ''}`} />
            </button>
            {open && (
                <div className="absolute bottom-full left-0 z-40 mb-2 w-96 max-w-[calc(100vw-2rem)] overflow-hidden rounded-2xl border border-border bg-panel/95 shadow-xl backdrop-blur">
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

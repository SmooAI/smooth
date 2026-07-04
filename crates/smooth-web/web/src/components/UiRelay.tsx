// SEP Phase 6 — extension ui/* relay surface for smooth-web.
//
// Opens its own /ws connection and renders the ui/* frames a dispatched
// operative's extension emits (relayed by the daemon as `UiRequest` /
// `UiResolved` events — see crates/smooth-bigsmooth/src/ui_relay.rs):
//   select | confirm | input  → modal dialog; answer POSTs /api/ui/answer
//   notify                    → toast (auto-expires)
//   set_status | set_title    → chrome (status pill / document title)
//   set_widget                → RenderBlock panel docked bottom-right
//
// Mounted once, globally, in Layout so a dialog surfaces on any page while a
// dispatch runs. Render-block kinds: markdown | keyvalue | progress | table |
// diff | stack, each with the mandatory `text` fallback.
import { useEffect, useRef, useState } from 'react';
import ReactMarkdown from 'react-markdown';
import remarkGfm from 'remark-gfm';

import { apiPost } from '../api';

type Interactive = {
    request_id: string;
    ext: string;
    kind: 'select' | 'confirm' | 'input';
    prompt: string;
    options: string[];
    default?: string;
};

type Toast = { id: number; message: string; level: string };

// ── RenderBlock (the set_widget payload) ────────────────────────────────
// Mirrors smooth_code::sep_host::RenderBlock but covers the web-only kinds
// (table/diff/stack) too. Every kind falls back to `text` for unknown shapes.
function RenderBlock({ widget }: { widget: Record<string, unknown> }) {
    const kind = typeof widget.kind === 'string' ? widget.kind : '';
    const text = typeof widget.text === 'string' ? widget.text : '';

    switch (kind) {
        case 'markdown': {
            const md = typeof widget.markdown === 'string' ? widget.markdown : text;
            return (
                <div className="prose prose-sm prose-invert max-w-none text-sm">
                    <ReactMarkdown remarkPlugins={[remarkGfm]}>{md}</ReactMarkdown>
                </div>
            );
        }
        case 'keyvalue': {
            const title = typeof widget.title === 'string' ? widget.title : undefined;
            const rows = Array.isArray(widget.rows) ? (widget.rows as Array<Record<string, unknown>>) : [];
            return (
                <div className="text-sm">
                    {title && <div className="font-semibold mb-1">{title}</div>}
                    <div className="grid grid-cols-[auto_1fr] gap-x-3 gap-y-0.5">
                        {rows.map((r, i) => (
                            <div key={i} className="contents">
                                <span className="text-muted-foreground">{String(r.key ?? '')}</span>
                                <span>{String(r.value ?? '')}</span>
                            </div>
                        ))}
                    </div>
                </div>
            );
        }
        case 'progress': {
            const value = typeof widget.value === 'number' ? Math.max(0, Math.min(1, widget.value)) : 0;
            const label = typeof widget.label === 'string' ? widget.label : undefined;
            return (
                <div className="text-sm">
                    {label && <div className="mb-1">{label}</div>}
                    <div className="h-2 w-full rounded bg-muted overflow-hidden">
                        <div className="h-full bg-primary" style={{ width: `${value * 100}%` }} />
                    </div>
                    <div className="text-xs text-muted-foreground mt-0.5">{Math.round(value * 100)}%</div>
                </div>
            );
        }
        case 'table': {
            // Phase 8 DSL uses `columns`; accept the pre-Phase-8 `headers` too.
            const headerSrc = Array.isArray(widget.columns) ? widget.columns : Array.isArray(widget.headers) ? widget.headers : [];
            const headers = (headerSrc as unknown[]).map(String);
            const rows = Array.isArray(widget.rows) ? (widget.rows as unknown[][]) : [];
            return (
                <div className="overflow-x-auto">
                    <table className="text-sm border-collapse">
                        {headers.length > 0 && (
                            <thead>
                                <tr>
                                    {headers.map((h, i) => (
                                        <th key={i} className="text-left font-semibold border-b border-border px-2 py-1">
                                            {h}
                                        </th>
                                    ))}
                                </tr>
                            </thead>
                        )}
                        <tbody>
                            {rows.map((row, i) => (
                                <tr key={i}>
                                    {(Array.isArray(row) ? row : [row]).map((cell, j) => (
                                        <td key={j} className="border-b border-border/50 px-2 py-1">
                                            {String(cell ?? '')}
                                        </td>
                                    ))}
                                </tr>
                            ))}
                        </tbody>
                    </table>
                </div>
            );
        }
        case 'diff': {
            // Phase 8 DSL uses `patch`; accept the pre-Phase-8 `diff` too.
            const diff = typeof widget.patch === 'string' ? widget.patch : typeof widget.diff === 'string' ? widget.diff : text;
            return (
                <pre className="text-xs font-mono overflow-x-auto">
                    {diff.split('\n').map((line, i) => {
                        const cls = line.startsWith('+') ? 'text-emerald-400' : line.startsWith('-') ? 'text-rose-400' : 'text-muted-foreground';
                        return (
                            <div key={i} className={cls}>
                                {line || ' '}
                            </div>
                        );
                    })}
                </pre>
            );
        }
        case 'stack': {
            // Phase 8 DSL uses `children`; accept the pre-Phase-8 `items` too.
            const items = Array.isArray(widget.children) ? widget.children : Array.isArray(widget.items) ? widget.items : [];
            return (
                <div className="flex flex-col gap-2">
                    {(items as Array<Record<string, unknown>>).map((item, i) => (
                        <RenderBlock key={i} widget={item} />
                    ))}
                </div>
            );
        }
        case 'widget': {
            // Phase 8 interactive tier: render the body block plus a legend of the
            // declared keybindings. (Live key routing back to the extension needs
            // the daemon's widget/key relay — the render is available now.)
            const body = widget.body && typeof widget.body === 'object' ? (widget.body as Record<string, unknown>) : undefined;
            const keybindings = Array.isArray(widget.keybindings) ? (widget.keybindings as Array<Record<string, unknown>>) : [];
            return (
                <div className="text-sm flex flex-col gap-2">
                    {body ? <RenderBlock widget={body} /> : <div className="whitespace-pre-wrap">{text}</div>}
                    {keybindings.length > 0 && (
                        <div className="flex flex-wrap gap-1.5 text-xs text-muted-foreground">
                            {keybindings.map((k, i) => (
                                <span key={i} className="rounded border border-border px-1.5 py-0.5">
                                    <span className="font-mono">{String(k.key ?? '')}</span>
                                    {k.description ? <span className="ml-1">{String(k.description)}</span> : null}
                                </span>
                            ))}
                        </div>
                    )}
                </div>
            );
        }
        default:
            // Unknown / plain text kinds → the mandatory text fallback.
            return <div className="text-sm whitespace-pre-wrap">{text || JSON.stringify(widget)}</div>;
    }
}

export function UiRelay() {
    const [dialogs, setDialogs] = useState<Interactive[]>([]);
    const [toasts, setToasts] = useState<Toast[]>([]);
    const [status, setStatus] = useState<string>('');
    const [widget, setWidget] = useState<Record<string, unknown> | null>(null);
    const [widgetExt, setWidgetExt] = useState<string>('');
    const toastId = useRef(0);
    const [inputValue, setInputValue] = useState('');

    // The first queued interactive dialog is the one on screen.
    const active = dialogs[0] ?? null;

    // Seed the input field when a new input dialog surfaces.
    useEffect(() => {
        if (active?.kind === 'input') setInputValue(active.default ?? '');
    }, [active?.request_id, active?.kind, active?.default]);

    useEffect(() => {
        let ws: WebSocket | null = null;
        let alive = true;
        let reconnect: ReturnType<typeof setTimeout> | null = null;

        const open = () => {
            if (!alive) return;
            try {
                const proto = window.location.protocol === 'https:' ? 'wss' : 'ws';
                ws = new WebSocket(`${proto}://${window.location.host}/ws`);
            } catch {
                reconnect = setTimeout(open, 2000);
                return;
            }
            ws.onmessage = (e) => {
                let ev: Record<string, unknown>;
                try {
                    ev = JSON.parse(typeof e.data === 'string' ? e.data : '');
                } catch {
                    return;
                }
                if (ev?.type === 'UiResolved' && typeof ev.request_id === 'string') {
                    // Another client (or a timeout / auto-answer) resolved it.
                    setDialogs((prev) => prev.filter((d) => d.request_id !== ev.request_id));
                    return;
                }
                if (ev?.type !== 'UiRequest') return;
                const kind = String(ev.kind ?? '');
                const request_id = String(ev.request_id ?? '');
                const ext = String(ev.ext ?? '');
                const payload = (ev.payload ?? {}) as Record<string, unknown>;

                if (kind === 'select' || kind === 'confirm' || kind === 'input') {
                    setDialogs((prev) => [
                        ...prev,
                        {
                            request_id,
                            ext,
                            kind,
                            prompt: typeof payload.prompt === 'string' ? payload.prompt : '',
                            options: Array.isArray(payload.options) ? (payload.options as unknown[]).map(String) : [],
                            default: typeof payload.default === 'string' ? payload.default : undefined,
                        },
                    ]);
                } else if (kind === 'notify') {
                    toastId.current += 1;
                    const id = toastId.current;
                    setToasts((prev) => [...prev, { id, message: String(payload.message ?? ''), level: String(payload.level ?? 'info') }]);
                    setTimeout(() => setToasts((prev) => prev.filter((t) => t.id !== id)), 6000);
                } else if (kind === 'set_status') {
                    setStatus(String(payload.status ?? ''));
                } else if (kind === 'set_title') {
                    const t = String(payload.title ?? '');
                    if (t) document.title = `${t} — Smooth`;
                } else if (kind === 'set_widget') {
                    const w = (payload.widget ?? null) as Record<string, unknown> | null;
                    setWidget(w);
                    setWidgetExt(ext);
                }
            };
            ws.onclose = () => {
                if (alive) reconnect = setTimeout(open, 2000);
            };
            ws.onerror = () => ws?.close();
        };
        open();

        return () => {
            alive = false;
            if (reconnect) clearTimeout(reconnect);
            ws?.close();
        };
    }, []);

    const answer = (d: Interactive, body: Record<string, unknown>) => {
        setDialogs((prev) => prev.filter((x) => x.request_id !== d.request_id));
        void apiPost('/api/ui/answer', { request_id: d.request_id, ...body }).catch(() => {});
    };

    return (
        <>
            {/* Widget dock — latest set_widget (bottom-right, above toasts) */}
            {widget && (
                <div className="fixed bottom-4 right-4 z-40 w-80 max-w-[calc(100vw-2rem)] rounded-lg border border-border bg-card/95 backdrop-blur p-3 shadow-lg">
                    <div className="flex items-center justify-between mb-2">
                        <span className="text-xs font-semibold uppercase tracking-wider text-muted-foreground">{widgetExt || 'extension'}</span>
                        <button className="text-muted-foreground hover:text-foreground text-xs" onClick={() => setWidget(null)} aria-label="Dismiss widget">
                            ✕
                        </button>
                    </div>
                    <RenderBlock widget={widget} />
                    {status && <div className="mt-2 pt-2 border-t border-border/50 text-xs text-muted-foreground">{status}</div>}
                </div>
            )}

            {/* Status pill when there's a status but no widget to host it */}
            {status && !widget && (
                <div className="fixed bottom-4 right-4 z-40 rounded-full border border-border bg-card/95 backdrop-blur px-3 py-1.5 text-xs text-muted-foreground shadow">
                    {status}
                </div>
            )}

            {/* Toasts */}
            <div className="fixed bottom-4 left-1/2 -translate-x-1/2 z-50 flex flex-col gap-2 items-center">
                {toasts.map((t) => (
                    <div
                        key={t.id}
                        className={
                            'rounded-md px-4 py-2 text-sm shadow-lg border ' +
                            (t.level === 'error'
                                ? 'bg-destructive/90 text-destructive-foreground border-destructive'
                                : t.level === 'warn' || t.level === 'warning'
                                  ? 'bg-amber-500/90 text-black border-amber-600'
                                  : 'bg-card/95 text-foreground border-border')
                        }
                    >
                        {t.message}
                    </div>
                ))}
            </div>

            {/* Interactive modal */}
            {active && (
                <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50 backdrop-blur-sm p-4">
                    <div className="w-full max-w-md rounded-xl border border-border bg-card p-5 shadow-xl">
                        <div className="text-xs font-semibold uppercase tracking-wider text-muted-foreground mb-2">{active.ext}</div>
                        <div className="text-sm mb-4 whitespace-pre-wrap">{active.prompt}</div>

                        {active.kind === 'select' && (
                            <div className="flex flex-col gap-2">
                                {active.options.map((opt, i) => (
                                    <button
                                        key={i}
                                        className="w-full text-left rounded-md border border-border px-3 py-2 text-sm hover:bg-primary/10 hover:border-primary transition-colors"
                                        onClick={() => answer(active, { value: opt })}
                                    >
                                        {opt}
                                    </button>
                                ))}
                            </div>
                        )}

                        {active.kind === 'confirm' && (
                            <div className="flex justify-end gap-2">
                                <button
                                    className="rounded-md border border-border px-4 py-2 text-sm hover:bg-muted transition-colors"
                                    onClick={() => answer(active, { confirmed: false })}
                                >
                                    No
                                </button>
                                <button
                                    className="rounded-md bg-primary px-4 py-2 text-sm text-primary-foreground hover:bg-primary/90 transition-colors"
                                    onClick={() => answer(active, { confirmed: true })}
                                >
                                    Yes
                                </button>
                            </div>
                        )}

                        {active.kind === 'input' && (
                            <form
                                onSubmit={(e) => {
                                    e.preventDefault();
                                    answer(active, { text: inputValue });
                                }}
                            >
                                <input
                                    autoFocus
                                    className="w-full rounded-md border border-border bg-background px-3 py-2 text-sm mb-4 focus:border-primary focus:outline-none"
                                    value={inputValue}
                                    onChange={(e) => setInputValue(e.target.value)}
                                />
                                <div className="flex justify-end gap-2">
                                    <button
                                        type="button"
                                        className="rounded-md border border-border px-4 py-2 text-sm hover:bg-muted transition-colors"
                                        onClick={() => answer(active, { cancelled: true })}
                                    >
                                        Cancel
                                    </button>
                                    <button
                                        type="submit"
                                        className="rounded-md bg-primary px-4 py-2 text-sm text-primary-foreground hover:bg-primary/90 transition-colors"
                                    >
                                        Submit
                                    </button>
                                </div>
                            </form>
                        )}

                        {active.kind !== 'input' && (
                            <button
                                className="mt-4 w-full text-center text-xs text-muted-foreground hover:text-foreground"
                                onClick={() => answer(active, { cancelled: true })}
                            >
                                Dismiss
                            </button>
                        )}
                    </div>
                </div>
            )}
        </>
    );
}

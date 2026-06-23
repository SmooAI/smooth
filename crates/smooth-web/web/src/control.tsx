// The smooth-daemon control surface (EPIC th-c89c2a / th-bd0def).
//
// A self-contained always-on-agent UI: sessions, a live-streaming chat, and an
// inline approval inbox (the daemon's PermissionRequest/Reply). Talks only to
// the daemon API via daemon.ts; deliberately independent of the legacy Big
// Smooth app so the two don't entangle.

import { useEffect, useReducer, useRef, useState } from 'react';

import { createSession, DaemonSocket, getHealth, listSessions, type Health, type ServerEvent, type Session } from './daemon';

type ChatItem =
    | { kind: 'user'; text: string }
    | { kind: 'assistant'; text: string }
    | { kind: 'tool'; name: string; args: string; result?: string; error?: boolean }
    | { kind: 'error'; text: string };

interface PendingApproval {
    request_id: string;
    tool_name: string;
    summary: string;
}

export function ControlApp() {
    const [health, setHealth] = useState<Health | null>(null);
    const [connected, setConnected] = useState(false);
    const [sessionId, setSessionId] = useState<string | null>(null);
    const [sessions, setSessions] = useState<Session[]>([]);
    const [items, setItems] = useState<ChatItem[]>([]);
    const [pending, setPending] = useState<PendingApproval[]>([]);
    const [busy, setBusy] = useState(false);
    const [input, setInput] = useState('');
    const socketRef = useRef<DaemonSocket | null>(null);
    const [, forceTick] = useReducer((n: number) => n + 1, 0);

    const refreshSessions = () => {
        listSessions()
            .then(setSessions)
            .catch(() => {});
    };

    // Single WebSocket for the lifetime of the surface; the handler closes over
    // the stable state setters.
    const handlerRef = useRef<(ev: ServerEvent) => void>(() => {});
    handlerRef.current = (ev: ServerEvent) => {
        switch (ev.type) {
            case 'Connected':
                setSessionId(ev.session_id);
                break;
            case 'TokenDelta':
                setItems((prev) => {
                    const last = prev[prev.length - 1];
                    if (last && last.kind === 'assistant') {
                        return [...prev.slice(0, -1), { kind: 'assistant', text: last.text + ev.content }];
                    }
                    return [...prev, { kind: 'assistant', text: ev.content }];
                });
                break;
            case 'ToolCallStart':
                setItems((prev) => [...prev, { kind: 'tool', name: ev.tool_name, args: ev.arguments }]);
                break;
            case 'ToolCallComplete':
                setItems((prev) => {
                    const copy = [...prev];
                    for (let i = copy.length - 1; i >= 0; i--) {
                        const it = copy[i];
                        if (it.kind === 'tool' && it.name === ev.tool_name && it.result === undefined) {
                            copy[i] = { ...it, result: ev.result, error: ev.is_error };
                            break;
                        }
                    }
                    return copy;
                });
                break;
            case 'TaskComplete':
                setBusy(false);
                refreshSessions();
                break;
            case 'TaskError':
                setItems((prev) => [...prev, { kind: 'error', text: ev.message }]);
                setBusy(false);
                break;
            case 'PermissionRequest':
                setPending((prev) => [...prev, { request_id: ev.request_id, tool_name: ev.tool_name, summary: ev.summary }]);
                break;
            default:
                break;
        }
    };

    useEffect(() => {
        getHealth().then(setHealth).catch(() => {});
        refreshSessions();
        const sock = new DaemonSocket(
            (ev) => handlerRef.current(ev),
            setConnected,
        );
        sock.connect();
        socketRef.current = sock;
        const poll = setInterval(refreshSessions, 5000);
        return () => {
            clearInterval(poll);
            sock.close();
        };
    }, []);

    const send = () => {
        const message = input.trim();
        if (!message || busy) return;
        setItems((prev) => [...prev, { kind: 'user', text: message }]);
        socketRef.current?.send({ type: 'TaskStart', message });
        setInput('');
        setBusy(true);
        forceTick();
    };

    const reply = (request_id: string, allow: boolean) => {
        socketRef.current?.send({ type: 'PermissionReply', request_id, allow });
        setPending((prev) => prev.filter((p) => p.request_id !== request_id));
    };

    const newSession = async () => {
        try {
            await createSession();
            refreshSessions();
        } catch {
            /* ignore */
        }
    };

    return (
        <div className="flex h-screen flex-col bg-background text-foreground">
            <header className="flex items-center justify-between border-b border-white/10 px-4 py-2">
                <div className="flex items-center gap-2">
                    <span className="text-lg font-semibold text-primary">Smooth</span>
                    <span className="text-xs text-foreground/50">daemon {health?.version ?? '—'}</span>
                </div>
                <div className="flex items-center gap-2 text-xs">
                    <span className={`h-2 w-2 rounded-full ${connected ? 'bg-primary' : 'bg-red-500'}`} />
                    <span className="text-foreground/60">{connected ? 'connected' : 'reconnecting…'}</span>
                </div>
            </header>

            <div className="flex min-h-0 flex-1">
                <aside className="w-60 shrink-0 overflow-y-auto border-r border-white/10 p-3">
                    <div className="mb-2 flex items-center justify-between">
                        <span className="text-xs uppercase tracking-wide text-foreground/40">Sessions</span>
                        <button onClick={newSession} className="rounded bg-primary/20 px-2 py-0.5 text-xs text-primary hover:bg-primary/30">
                            + new
                        </button>
                    </div>
                    <ul className="space-y-1">
                        {sessions.map((s) => (
                            <li
                                key={s.id}
                                className={`truncate rounded px-2 py-1 text-xs ${s.id === sessionId ? 'bg-primary/15 text-foreground' : 'text-foreground/60'}`}
                                title={s.id}
                            >
                                <span className={`mr-1 inline-block h-1.5 w-1.5 rounded-full ${s.status === 'active' ? 'bg-primary' : 'bg-foreground/30'}`} />
                                {s.title ?? s.id.slice(0, 8)}
                            </li>
                        ))}
                        {sessions.length === 0 && <li className="text-xs text-foreground/30">no sessions yet</li>}
                    </ul>
                </aside>

                <main className="flex min-w-0 flex-1 flex-col">
                    <div className="flex-1 space-y-3 overflow-y-auto p-4">
                        {items.length === 0 && <div className="text-sm text-foreground/30">Ask the daemon to do something…</div>}
                        {items.map((it, i) => (
                            <ChatBubble key={i} item={it} />
                        ))}
                    </div>

                    {pending.length > 0 && (
                        <div className="space-y-2 border-t border-amber-500/30 bg-amber-500/5 p-3">
                            {pending.map((p) => (
                                <div key={p.request_id} className="flex items-center justify-between gap-3 text-sm">
                                    <div className="min-w-0">
                                        <span className="font-medium text-amber-300">approve {p.tool_name}?</span>{' '}
                                        <span className="truncate text-foreground/60">{p.summary}</span>
                                    </div>
                                    <div className="flex shrink-0 gap-2">
                                        <button onClick={() => reply(p.request_id, true)} className="rounded bg-primary/20 px-3 py-1 text-xs text-primary hover:bg-primary/30">
                                            allow
                                        </button>
                                        <button onClick={() => reply(p.request_id, false)} className="rounded bg-red-500/20 px-3 py-1 text-xs text-red-300 hover:bg-red-500/30">
                                            deny
                                        </button>
                                    </div>
                                </div>
                            ))}
                        </div>
                    )}

                    <div className="flex gap-2 border-t border-white/10 p-3">
                        <input
                            value={input}
                            onChange={(e) => setInput(e.target.value)}
                            onKeyDown={(e) => e.key === 'Enter' && send()}
                            placeholder={busy ? 'running…' : 'message the daemon'}
                            disabled={busy}
                            className="flex-1 rounded border border-white/10 bg-white/5 px-3 py-2 text-sm outline-none focus:border-primary/50"
                        />
                        <button onClick={send} disabled={busy || !input.trim()} className="rounded bg-primary px-4 py-2 text-sm font-medium text-background disabled:opacity-40">
                            send
                        </button>
                    </div>
                </main>
            </div>
        </div>
    );
}

function ChatBubble({ item }: { item: ChatItem }) {
    if (item.kind === 'user') {
        return <div className="ml-auto max-w-[80%] rounded-lg bg-primary/15 px-3 py-2 text-sm">{item.text}</div>;
    }
    if (item.kind === 'assistant') {
        return <div className="max-w-[80%] whitespace-pre-wrap rounded-lg bg-white/5 px-3 py-2 text-sm">{item.text}</div>;
    }
    if (item.kind === 'error') {
        return <div className="max-w-[80%] rounded-lg bg-red-500/10 px-3 py-2 text-sm text-red-300">{item.text}</div>;
    }
    // tool
    return (
        <div className="max-w-[80%] rounded-lg border border-white/10 bg-black/20 px-3 py-2 font-mono text-xs">
            <div className="text-primary/80">
                {item.name} <span className="text-foreground/40">{item.args.slice(0, 120)}</span>
            </div>
            {item.result !== undefined && <div className={`mt-1 whitespace-pre-wrap ${item.error ? 'text-red-300' : 'text-foreground/50'}`}>{item.result.slice(0, 400)}</div>}
        </div>
    );
}

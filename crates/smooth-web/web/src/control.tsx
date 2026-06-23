// The smooth-daemon control surface (EPIC th-c89c2a / th-bd0def).
//
// A self-contained always-on-agent UI: sessions, a live-streaming chat, and an
// inline approval inbox (the daemon's PermissionRequest/Reply). Talks only to
// the daemon API via daemon.ts; deliberately independent of the legacy Big
// Smooth app so the two don't entangle.

import { useEffect, useReducer, useRef, useState } from 'react';
import ReactMarkdown from 'react-markdown';

import {
    createSession,
    DaemonSocket,
    getHealth,
    getStatus,
    listMessages,
    listSessions,
    PERMISSION_MODES,
    searchMemory,
    setMode,
    type Health,
    type MemoryHit,
    type PermissionMode,
    type ServerEvent,
    type Session,
    type Status,
} from './daemon';

type ChatItem =
    | { kind: 'user'; text: string }
    | { kind: 'assistant'; text: string }
    | { kind: 'tool'; name: string; args: string; result?: string; error?: boolean }
    | { kind: 'complete'; iterations: number; cost_usd: number }
    | { kind: 'error'; text: string };

interface PendingApproval {
    request_id: string;
    tool_name: string;
    summary: string;
}

export function ControlApp() {
    const [health, setHealth] = useState<Health | null>(null);
    const [status, setStatus] = useState<Status | null>(null);
    const [connected, setConnected] = useState(false);
    const [sessionId, setSessionId] = useState<string | null>(null);
    const [sessions, setSessions] = useState<Session[]>([]);
    const [items, setItems] = useState<ChatItem[]>([]);
    const [pending, setPending] = useState<PendingApproval[]>([]);
    const [busy, setBusy] = useState(false);
    const [taskId, setTaskId] = useState<string | null>(null);
    const [input, setInput] = useState('');
    const [memQuery, setMemQuery] = useState('');
    const [memHits, setMemHits] = useState<MemoryHit[] | null>(null);
    const socketRef = useRef<DaemonSocket | null>(null);
    const scrollRef = useRef<HTMLDivElement | null>(null);
    const [, forceTick] = useReducer((n: number) => n + 1, 0);

    const refreshSessions = () => {
        listSessions()
            .then(setSessions)
            .catch(() => {});
        getStatus()
            .then(setStatus)
            .catch(() => {});
    };

    // Single WebSocket for the lifetime of the surface; the handler closes over
    // the stable state setters.
    const handlerRef = useRef<(ev: ServerEvent) => void>(() => {});
    handlerRef.current = (ev: ServerEvent) => {
        // Learn the running task's id from the first event that carries it, so
        // we can cancel it.
        if ('task_id' in ev && ev.task_id) setTaskId(ev.task_id);
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
                setItems((prev) => [...prev, { kind: 'complete', iterations: ev.iterations, cost_usd: ev.cost_usd }]);
                setBusy(false);
                setTaskId(null);
                refreshSessions();
                break;
            case 'TaskError':
                setItems((prev) => [...prev, { kind: 'error', text: ev.message }]);
                setBusy(false);
                setTaskId(null);
                refreshSessions();
                break;
            case 'PermissionRequest':
                setPending((prev) => [...prev, { request_id: ev.request_id, tool_name: ev.tool_name, summary: ev.summary }]);
                break;
            default:
                break;
        }
    };

    // (Re)connect the single socket, optionally resuming a session so its
    // durable history replays on the next turn.
    const connect = (resume?: string) => {
        socketRef.current?.close();
        const sock = new DaemonSocket((ev) => handlerRef.current(ev), setConnected, resume);
        sock.connect();
        socketRef.current = sock;
    };

    useEffect(() => {
        getHealth().then(setHealth).catch(() => {});
        refreshSessions();
        connect();
        const poll = setInterval(refreshSessions, 5000);
        return () => {
            clearInterval(poll);
            socketRef.current?.close();
        };
        // eslint-disable-next-line react-hooks/exhaustive-deps
    }, []);

    // Keep the chat pinned to the newest item as tokens/tools stream in.
    useEffect(() => {
        const el = scrollRef.current;
        if (el) el.scrollTop = el.scrollHeight;
    }, [items]);

    // Switch to an existing session: load its conversation history and resume.
    const selectSession = async (id: string) => {
        if (id === sessionId) return;
        setBusy(false);
        setTaskId(null);
        setPending([]);
        try {
            const history = await listMessages(id);
            setItems(history.map((m) => (m.role === 'user' ? { kind: 'user', text: m.content } : { kind: 'assistant', text: m.content })));
        } catch {
            setItems([]);
        }
        setSessionId(id);
        connect(id);
    };

    const send = () => {
        const message = input.trim();
        if (!message || busy) return;
        setItems((prev) => [...prev, { kind: 'user', text: message }]);
        socketRef.current?.send({ type: 'TaskStart', message });
        setInput('');
        setBusy(true);
        forceTick();
    };

    // Switch the daemon's permission posture, then refresh status so the
    // header reflects the resolved mode (and any concurrent change).
    const changeMode = async (mode: PermissionMode) => {
        setStatus((prev) => (prev ? { ...prev, permission_mode: mode } : prev));
        try {
            await setMode(mode);
        } finally {
            getStatus().then(setStatus).catch(() => {});
        }
    };

    const cancel = () => {
        if (taskId) socketRef.current?.send({ type: 'TaskCancel', task_id: taskId });
        // The daemon emits a terminal TaskError back, which flips busy off; this
        // is just an optimistic UI nudge in case the socket is mid-reconnect.
        setBusy(false);
    };

    const runMemorySearch = async () => {
        const q = memQuery.trim();
        if (!q) {
            setMemHits(null);
            return;
        }
        try {
            setMemHits(await searchMemory(q));
        } catch {
            setMemHits([]);
        }
    };

    const reply = (request_id: string, allow: boolean) => {
        socketRef.current?.send({ type: 'PermissionReply', request_id, allow });
        setPending((prev) => prev.filter((p) => p.request_id !== request_id));
    };

    const newSession = async () => {
        try {
            const s = await createSession();
            setItems([]);
            setPending([]);
            setSessionId(s.id);
            connect(s.id);
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
                <div className="flex items-center gap-3 text-xs">
                    {status && (
                        <select
                            value={status.permission_mode}
                            onChange={(e) => void changeMode(e.target.value as PermissionMode)}
                            title="permission mode — takes effect on the next task"
                            className="cursor-pointer rounded border border-white/10 bg-white/5 px-2 py-0.5 font-mono text-foreground/70 outline-none focus:border-primary/50"
                        >
                            {PERMISSION_MODES.map((m) => (
                                <option key={m} value={m} className="bg-background text-foreground">
                                    {m}
                                </option>
                            ))}
                        </select>
                    )}
                    {status && (
                        <span
                            title={status.egress_proxy ? `egress confined via ${status.egress_proxy}` : 'egress unrestricted'}
                            className={`rounded px-2 py-0.5 ${status.egress_proxy ? 'bg-primary/15 text-primary' : 'bg-white/5 text-foreground/40'}`}
                        >
                            egress {status.egress_proxy ? 'on' : 'off'}
                        </span>
                    )}
                    {status && status.active_tasks > 0 && <span className="text-foreground/50">{status.active_tasks} running</span>}
                    <span className="flex items-center gap-1">
                        <span className={`h-2 w-2 rounded-full ${connected ? 'bg-primary' : 'bg-red-500'}`} />
                        <span className="text-foreground/60">{connected ? 'connected' : 'reconnecting…'}</span>
                    </span>
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
                            <li key={s.id}>
                                <button
                                    onClick={() => void selectSession(s.id)}
                                    title={s.id}
                                    className={`flex w-full items-center rounded px-2 py-1 text-left text-xs hover:bg-white/5 ${s.id === sessionId ? 'bg-primary/15 text-foreground' : 'text-foreground/60'}`}
                                >
                                    <span className={`mr-1 inline-block h-1.5 w-1.5 shrink-0 rounded-full ${s.status === 'active' ? 'bg-primary' : 'bg-foreground/30'}`} />
                                    <span className="truncate">{s.title ?? s.id.slice(0, 8)}</span>
                                </button>
                            </li>
                        ))}
                        {sessions.length === 0 && <li className="text-xs text-foreground/30">no sessions yet</li>}
                    </ul>

                    <div className="mt-4 border-t border-white/10 pt-3">
                        <span className="text-xs uppercase tracking-wide text-foreground/40">Memory</span>
                        <input
                            value={memQuery}
                            onChange={(e) => setMemQuery(e.target.value)}
                            onKeyDown={(e) => e.key === 'Enter' && void runMemorySearch()}
                            placeholder="search memory…"
                            className="mt-2 w-full rounded border border-white/10 bg-white/5 px-2 py-1 text-xs outline-none focus:border-primary/50"
                        />
                        {memHits !== null && (
                            <ul className="mt-2 space-y-1">
                                {memHits.length === 0 && <li className="text-xs text-foreground/30">no matches</li>}
                                {memHits.map((m, i) => (
                                    <li key={i} className="rounded bg-white/5 px-2 py-1 text-xs">
                                        <span className="mr-1 rounded bg-primary/15 px-1 text-[10px] text-primary">{m.memory_type}</span>
                                        <span className="text-foreground/70">{m.content}</span>
                                    </li>
                                ))}
                            </ul>
                        )}
                    </div>
                </aside>

                <main className="flex min-w-0 flex-1 flex-col">
                    <div ref={scrollRef} className="flex-1 space-y-3 overflow-y-auto p-4">
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
                        {busy ? (
                            <button onClick={cancel} className="rounded bg-red-500/20 px-4 py-2 text-sm font-medium text-red-300 hover:bg-red-500/30">
                                stop
                            </button>
                        ) : (
                            <button onClick={send} disabled={!input.trim()} className="rounded bg-primary px-4 py-2 text-sm font-medium text-background disabled:opacity-40">
                                send
                            </button>
                        )}
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
        return (
            <div className="max-w-[80%] space-y-2 rounded-lg bg-white/5 px-3 py-2 text-sm [&_a]:text-primary [&_a]:underline [&_code]:rounded [&_code]:bg-black/30 [&_code]:px-1 [&_code]:py-0.5 [&_code]:font-mono [&_code]:text-xs [&_li]:ml-4 [&_li]:list-disc [&_ol_li]:list-decimal [&_pre]:overflow-x-auto [&_pre]:rounded [&_pre]:bg-black/30 [&_pre]:p-2 [&_pre_code]:bg-transparent [&_pre_code]:p-0">
                <ReactMarkdown>{item.text}</ReactMarkdown>
            </div>
        );
    }
    if (item.kind === 'complete') {
        return (
            <div className="text-xs text-foreground/40">
                done · {item.iterations} iteration{item.iterations === 1 ? '' : 's'} · ${item.cost_usd.toFixed(4)}
            </div>
        );
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

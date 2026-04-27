import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { ArrowLeft, Plus, Send, Square, Trash2, Users } from 'lucide-react';
import ReactMarkdown from 'react-markdown';
import remarkGfm from 'remark-gfm';
import { api } from '../api';
import { BigSmoothFace } from '../components/BigSmoothFace';
import { useIsMobile } from '../hooks/use-mobile';

// Slash commands surfaced when the user types `/` at the start of the input.
// Each `expand` runs when the command is accepted; the returned string
// replaces the input contents (or null to leave the input alone, e.g. /clear).
const SLASH_COMMANDS: { name: string; description: string; expand: () => string | null }[] = [
    { name: '/help', description: 'List available commands', expand: () => 'Show me what slash commands and mentions are supported in this chat.' },
    { name: '/pearls', description: 'List open pearls', expand: () => 'List my open pearls.' },
    { name: '/teammates', description: 'List active teammates', expand: () => 'Which teammates are running right now?' },
    { name: '/spawn', description: 'Spawn a teammate on a new task', expand: () => 'Create a pearl for ' },
    { name: '/status', description: 'Show overall status', expand: () => 'Give me a quick status — pearls open vs in progress, active teammates, anything stuck.' },
    { name: '/clear', description: 'Clear the input', expand: () => '' },
];

interface SearchResult {
    kind: string; // "pearl" | "file" | "path"
    label: string;
    detail?: string;
    insert: string;
}

interface Session {
    id: string;
    title: string;
    model: string;
    started_at: string;
    message_count: number;
}

interface Msg {
    id: string;
    role: string;
    content: string;
}

interface Teammate {
    name: string;
    pearl_id: string;
    title: string;
    status: string; // running | idle | ended
    started_at: string;
    last_event_at: string;
}

interface TeammateMessage {
    id: string;
    session_id: string;
    from: string;
    to: string;
    content: string;
    timestamp: string;
}

interface Thought {
    id: number;
    text: string;
    bornAt: number;
}

// `null` selection means the user is chatting with Big Smooth (the lead).
type Scope = { kind: 'lead' } | { kind: 'teammate'; name: string };

function RelativeTime({ iso }: { iso: string }) {
    const then = new Date(iso).getTime();
    const now = Date.now();
    const s = Math.max(0, Math.floor((now - then) / 1000));
    let label = '';
    if (s < 60) label = 'just now';
    else if (s < 3600) label = `${Math.floor(s / 60)}m ago`;
    else if (s < 86_400) label = `${Math.floor(s / 3600)}h ago`;
    else label = `${Math.floor(s / 86_400)}d ago`;
    return <span>{label}</span>;
}

export function ChatPage() {
    const [sessions, setSessions] = useState<Session[]>([]);
    const [activeId, setActiveId] = useState<string | null>(null);
    const [messages, setMessages] = useState<Msg[]>([]);
    const [input, setInput] = useState('');
    const [streaming, setStreaming] = useState(false);
    const [loadingSessions, setLoadingSessions] = useState(true);
    const [teammates, setTeammates] = useState<Teammate[]>([]);
    const [scope, setScope] = useState<Scope>({ kind: 'lead' });
    const [teammateMessages, setTeammateMessages] = useState<TeammateMessage[]>([]);
    const [thoughts, setThoughts] = useState<Thought[]>([]);
    const bottomRef = useRef<HTMLDivElement>(null);
    const inputRef = useRef<HTMLInputElement>(null);
    const thoughtIdRef = useRef(0);
    const sendAbortRef = useRef<AbortController | null>(null);
    const isMobile = useIsMobile();

    // Autocomplete: slash commands on '/<...>', mentions on '@<...>'.
    // When `mode` is null the popover is hidden.
    const [acMode, setAcMode] = useState<'slash' | 'mention' | null>(null);
    const [acQuery, setAcQuery] = useState('');
    const [acResults, setAcResults] = useState<SearchResult[]>([]);
    const [acIndex, setAcIndex] = useState(0);

    const slashSuggestions = useMemo(() => {
        if (acMode !== 'slash') return [];
        const q = acQuery.toLowerCase();
        return SLASH_COMMANDS.filter((c) => c.name.toLowerCase().includes(q));
    }, [acMode, acQuery]);

    // Drive autocomplete from the current input value + caret position.
    const refreshAutocomplete = useCallback(
        async (value: string) => {
            // Slash commands: the input must START with `/` (and not contain
            // a space yet). Anything else closes the menu.
            if (value.startsWith('/') && !value.includes(' ')) {
                setAcMode('slash');
                setAcQuery(value);
                setAcIndex(0);
                return;
            }
            // Mentions: look back from the end of the buffer for an `@`,
            // bounded by whitespace, ≤ 30 chars.
            const m = value.match(/(?:^|\s)@([^\s]{0,30})$/);
            if (m) {
                setAcMode('mention');
                const q = m[1] ?? '';
                setAcQuery(q);
                setAcIndex(0);
                if (q.length === 0) {
                    setAcResults([]);
                    return;
                }
                try {
                    const r = await api<{ data: SearchResult[] }>(`/api/search?q=${encodeURIComponent(q)}`);
                    setAcResults((r.data || []).slice(0, 8));
                } catch {
                    setAcResults([]);
                }
                return;
            }
            setAcMode(null);
            setAcResults([]);
        },
        [],
    );

    const acceptAutocomplete = useCallback(() => {
        if (acMode === 'slash') {
            const cmd = slashSuggestions[acIndex];
            if (!cmd) return;
            const replacement = cmd.expand();
            if (replacement === null) return;
            setInput(replacement);
            setAcMode(null);
            inputRef.current?.focus();
            return;
        }
        if (acMode === 'mention') {
            const r = acResults[acIndex];
            if (!r) return;
            setInput((cur) => cur.replace(/@([^\s]{0,30})$/, `${r.insert} `));
            setAcMode(null);
            inputRef.current?.focus();
        }
    }, [acMode, acIndex, slashSuggestions, acResults]);

    const refreshSessions = useCallback(async () => {
        try {
            const r = await api<{ data: Session[] }>('/api/chat/sessions');
            setSessions(r.data || []);
        } catch {
            setSessions([]);
        } finally {
            setLoadingSessions(false);
        }
    }, []);

    const refreshTeammates = useCallback(async () => {
        try {
            const r = await api<{ data: Teammate[] }>('/api/teammates');
            setTeammates(r.data || []);
        } catch {
            setTeammates([]);
        }
    }, []);

    useEffect(() => {
        refreshSessions();
        refreshTeammates();
        const t = setInterval(refreshTeammates, 5000);
        return () => clearInterval(t);
    }, [refreshSessions, refreshTeammates]);

    // Lead-mode session messages.
    useEffect(() => {
        if (scope.kind !== 'lead') return;
        if (!activeId) {
            setMessages([]);
            return;
        }
        (async () => {
            try {
                const r = await api<{ data: Msg[] }>(`/api/chat/sessions/${activeId}/messages`);
                setMessages(r.data || []);
            } catch {
                setMessages([]);
            }
        })();
    }, [activeId, scope]);

    // Teammate-scoped pearl-comment thread, polled every 2s while focused.
    useEffect(() => {
        if (scope.kind !== 'teammate') return;
        let cancelled = false;
        const fetchOnce = async () => {
            try {
                const r = await api<{ data: TeammateMessage[] }>(`/api/teammates/${scope.name}/messages`);
                if (!cancelled) setTeammateMessages(r.data || []);
            } catch {
                if (!cancelled) setTeammateMessages([]);
            }
        };
        fetchOnce();
        const t = setInterval(fetchOnce, 2000);
        return () => {
            cancelled = true;
            clearInterval(t);
        };
    }, [scope]);

    useEffect(() => {
        bottomRef.current?.scrollIntoView({ behavior: 'smooth' });
    }, [messages.length, teammateMessages.length, streaming]);

    // Big Smooth's "thought stream": subscribe to /ws and surface
    // BigSmoothThought events as floating bubbles next to the face.
    // Older bubbles auto-expire so the row stays at most 3 deep.
    useEffect(() => {
        let ws: WebSocket | null = null;
        let alive = true;
        let reconnectTimer: ReturnType<typeof setTimeout> | null = null;

        const open = () => {
            if (!alive) return;
            try {
                const proto = window.location.protocol === 'https:' ? 'wss' : 'ws';
                ws = new WebSocket(`${proto}://${window.location.host}/ws`);
            } catch {
                reconnectTimer = setTimeout(open, 2000);
                return;
            }
            ws.onmessage = (e) => {
                try {
                    const ev = JSON.parse(typeof e.data === 'string' ? e.data : '');
                    if (ev?.type === 'BigSmoothThought' && typeof ev.text === 'string') {
                        thoughtIdRef.current += 1;
                        const id = thoughtIdRef.current;
                        const bornAt = Date.now();
                        setThoughts((prev) => [...prev, { id, text: ev.text, bornAt }].slice(-3));
                        // Auto-expire after 14s — long enough that a slow
                        // tool call doesn't leave the bubble row empty.
                        setTimeout(() => {
                            setThoughts((prev) => prev.filter((t) => t.id !== id));
                        }, 14000);
                    }
                } catch {
                    // ignore non-JSON / unknown frames
                }
            };
            ws.onclose = () => {
                if (!alive) return;
                reconnectTimer = setTimeout(open, 2000);
            };
            ws.onerror = () => {
                ws?.close();
            };
        };
        open();

        return () => {
            alive = false;
            if (reconnectTimer) clearTimeout(reconnectTimer);
            ws?.close();
        };
    }, []);

    // Clear thoughts when the user leaves the lead chat. We DON'T
    // clear on `streaming` flipping false anymore — the user wants to
    // be able to read what Big Smooth was thinking even after the
    // reply lands. The 14s auto-expire on each bubble already drains
    // the row organically.
    useEffect(() => {
        if (scope.kind !== 'lead') {
            const t = setTimeout(() => setThoughts([]), 1500);
            return () => clearTimeout(t);
        }
    }, [scope]);

    // Hotkeys: Shift+ArrowDown cycles lead → t1 → t2 → … → lead. Escape returns to lead.
    useEffect(() => {
        const handler = (e: KeyboardEvent) => {
            if (e.key === 'Escape') {
                setScope({ kind: 'lead' });
                return;
            }
            if (e.key === 'ArrowDown' && e.shiftKey) {
                e.preventDefault();
                setScope((cur) => {
                    if (teammates.length === 0) return { kind: 'lead' };
                    if (cur.kind === 'lead') return { kind: 'teammate', name: teammates[0].name };
                    const idx = teammates.findIndex((t) => t.name === cur.name);
                    if (idx < 0 || idx >= teammates.length - 1) return { kind: 'lead' };
                    return { kind: 'teammate', name: teammates[idx + 1].name };
                });
            }
        };
        window.addEventListener('keydown', handler);
        return () => window.removeEventListener('keydown', handler);
    }, [teammates]);

    const newChat = useCallback(async () => {
        try {
            const r = await api<{ data: Session }>('/api/chat/sessions', {
                method: 'POST',
                body: JSON.stringify({}),
            });
            setSessions((prev) => [r.data, ...prev]);
            setActiveId(r.data.id);
            setMessages([]);
            setScope({ kind: 'lead' });
        } catch (e) {
            console.error('new chat failed', e);
        }
    }, []);

    const deleteChat = useCallback(
        async (id: string) => {
            if (!confirm('Delete this chat?')) return;
            try {
                await api(`/api/chat/sessions/${id}`, { method: 'DELETE' });
                setSessions((prev) => prev.filter((s) => s.id !== id));
                if (activeId === id) {
                    setActiveId(null);
                    setMessages([]);
                }
            } catch (e) {
                console.error('delete failed', e);
            }
        },
        [activeId],
    );

    const sendToLead = useCallback(async () => {
        const content = input.trim();
        if (!content || streaming) return;
        let sessionId = activeId;
        if (!sessionId) {
            try {
                const r = await api<{ data: Session }>('/api/chat/sessions', {
                    method: 'POST',
                    body: JSON.stringify({}),
                });
                sessionId = r.data.id;
                setActiveId(sessionId);
                setSessions((prev) => [r.data, ...prev]);
            } catch (e) {
                console.error('session create failed', e);
                return;
            }
        }
        setInput('');
        setMessages((prev) => [...prev, { id: `tmp-${Date.now()}`, role: 'user', content }]);
        setStreaming(true);
        // AbortController so the user can hit Stop and reclaim the input.
        // Server-side the chat-agent task may keep running for a beat
        // (it's already mid-tool-call), but the UI is unblocked.
        const ctrl = new AbortController();
        sendAbortRef.current = ctrl;
        try {
            const resp = await fetch(`/api/chat/sessions/${sessionId}/messages`, {
                method: 'POST',
                headers: { 'Content-Type': 'application/json' },
                body: JSON.stringify({ content }),
                signal: ctrl.signal,
            });
            const json = await resp.json();
            const assistantMsg: Msg = {
                id: json.data?.id ?? `resp-${Date.now()}`,
                role: 'assistant',
                content: json.data?.content ?? 'No response',
            };
            setMessages((prev) => [...prev, assistantMsg]);
            refreshSessions();
        } catch (e) {
            const aborted = (e as Error).name === 'AbortError';
            if (!aborted) {
                setMessages((prev) => [...prev, { id: `err-${Date.now()}`, role: 'assistant', content: `Error: ${(e as Error).message}` }]);
            } else {
                setMessages((prev) => [...prev, { id: `cancel-${Date.now()}`, role: 'assistant', content: '_(stopped — Big Smooth was still working when you cancelled)_' }]);
            }
        } finally {
            sendAbortRef.current = null;
            setStreaming(false);
        }
    }, [input, streaming, activeId, refreshSessions]);

    const stopSend = useCallback(() => {
        sendAbortRef.current?.abort();
    }, []);

    const sendToTeammate = useCallback(async () => {
        if (scope.kind !== 'teammate') return;
        const content = input.trim();
        if (!content || streaming) return;
        setInput('');
        setStreaming(true);
        try {
            await fetch(`/api/teammates/${scope.name}/messages`, {
                method: 'POST',
                headers: { 'Content-Type': 'application/json' },
                body: JSON.stringify({ content }),
            });
            // Optimistic: re-fetch immediately so the new [CHAT:USER] shows up.
            const r = await api<{ data: TeammateMessage[] }>(`/api/teammates/${scope.name}/messages`);
            setTeammateMessages(r.data || []);
        } catch (e) {
            console.error('teammate send failed', e);
        } finally {
            setStreaming(false);
        }
    }, [input, streaming, scope]);

    const send = useCallback(() => {
        if (scope.kind === 'lead') sendToLead();
        else sendToTeammate();
    }, [scope, sendToLead, sendToTeammate]);

    const showListOnMobile = isMobile && scope.kind === 'lead' && !activeId;
    const showConvoOnMobile = isMobile && (scope.kind === 'teammate' || !!activeId);

    const Sidebar = (
        <aside
            className={`flex flex-col rounded-lg ${isMobile ? 'w-full flex-1 min-h-0' : 'w-64 shrink-0'}`}
            style={{ background: 'var(--smoo-dark-blue-850)', border: '1px solid var(--border)' }}
        >
            {/* Lead — Big Smooth */}
            <button
                type="button"
                onClick={() => setScope({ kind: 'lead' })}
                className="px-3 py-3 text-left border-b cursor-pointer transition-colors"
                style={{
                    borderColor: 'var(--border)',
                    background: scope.kind === 'lead' ? 'var(--smoo-green-alpha)' : 'transparent',
                }}
            >
                <div className="flex items-center gap-2">
                    <div className="w-2 h-2 rounded-full" style={{ background: 'var(--smoo-green)' }} />
                    <div className="font-semibold text-sm">Big Smooth</div>
                </div>
                <div className="text-[11px] mt-0.5" style={{ color: 'var(--muted)' }}>Lead · orchestrates work</div>
            </button>

            {/* Teammates */}
            <div className="flex flex-col border-b" style={{ borderColor: 'var(--border)' }}>
                <div className="px-3 py-2 flex items-center gap-2 text-xs uppercase tracking-wide" style={{ color: 'var(--muted)' }}>
                    <Users size={12} />
                    Teammates
                    <span className="ml-auto">{teammates.length}</span>
                </div>
                {teammates.length === 0 && (
                    <div className="px-3 pb-2 text-[11px] italic" style={{ color: 'var(--muted)' }}>
                        None active. Ask Big Smooth to spawn one.
                    </div>
                )}
                {teammates.map((t) => {
                    const selected = scope.kind === 'teammate' && scope.name === t.name;
                    const dotColor = t.status === 'idle' ? 'var(--muted)' : t.status === 'ended' ? '#777' : 'var(--smoo-orange)';
                    return (
                        <button
                            key={t.name}
                            type="button"
                            onClick={() => setScope({ kind: 'teammate', name: t.name })}
                            className="px-3 py-2 text-left cursor-pointer transition-colors hover:bg-white/5"
                            style={{
                                background: selected ? 'var(--smoo-green-alpha)' : 'transparent',
                            }}
                        >
                            <div className="flex items-center gap-2">
                                <div className="w-2 h-2 rounded-full" style={{ background: dotColor }} />
                                <div className="text-sm font-mono truncate flex-1" title={t.name}>{t.name}</div>
                            </div>
                            <div className="text-[11px] mt-0.5 truncate" style={{ color: 'var(--muted)' }}>{t.title}</div>
                            <div className="text-[10px] mt-0.5" style={{ color: 'var(--muted)' }}>
                                {t.status} · <RelativeTime iso={t.last_event_at} />
                            </div>
                        </button>
                    );
                })}
            </div>

            {/* Chats with Big Smooth (only meaningful in lead mode) */}
            {scope.kind === 'lead' && (
                <>
                    <div className="px-3 py-2 flex items-center justify-between text-xs uppercase tracking-wide border-b" style={{ color: 'var(--muted)', borderColor: 'var(--border)' }}>
                        <span>Chats</span>
                        <button
                            onClick={() => newChat()}
                            type="button"
                            className="rounded hover:bg-white/5 cursor-pointer min-h-[24px] min-w-[24px] flex items-center justify-center"
                            title="New chat"
                            aria-label="New chat"
                        >
                            <Plus size={14} />
                        </button>
                    </div>
                    <div className="flex-1 overflow-auto">
                        {loadingSessions && (
                            <div className="p-3 text-xs" style={{ color: 'var(--muted)' }}>Loading…</div>
                        )}
                        {!loadingSessions && sessions.length === 0 && (
                            <div className="p-3 text-xs" style={{ color: 'var(--muted)' }}>
                                No chats yet. Tap + to start one.
                            </div>
                        )}
                        {sessions.map((s) => (
                            <div
                                key={s.id}
                                onClick={() => setActiveId(s.id)}
                                className={`px-3 py-3 cursor-pointer border-b group ${activeId === s.id ? '' : 'hover:bg-white/5'}`}
                                style={{
                                    borderColor: 'var(--border)',
                                    background: activeId === s.id ? 'var(--smoo-green-alpha)' : 'transparent',
                                }}
                            >
                                <div className="flex items-center justify-between gap-2">
                                    <div className="text-sm font-medium truncate flex-1">{s.title || 'Untitled'}</div>
                                    <button
                                        onClick={(e) => {
                                            e.stopPropagation();
                                            deleteChat(s.id);
                                        }}
                                        type="button"
                                        className={`rounded hover:bg-white/10 cursor-pointer shrink-0 min-h-[36px] min-w-[36px] flex items-center justify-center ${
                                            isMobile ? '' : 'opacity-0 group-hover:opacity-100'
                                        }`}
                                        title="Delete chat"
                                        aria-label="Delete chat"
                                    >
                                        <Trash2 size={14} />
                                    </button>
                                </div>
                                <div className="text-[11px] flex items-center gap-2 mt-0.5" style={{ color: 'var(--muted)' }}>
                                    <span>{s.message_count} msg{s.message_count === 1 ? '' : 's'}</span>
                                    <span>·</span>
                                    <RelativeTime iso={s.started_at} />
                                </div>
                            </div>
                        ))}
                    </div>
                </>
            )}
        </aside>
    );

    const conversationTitle = useMemo(() => {
        if (scope.kind === 'lead') return 'Chat with Big Smooth';
        const t = teammates.find((x) => x.name === scope.name);
        return t ? `Chat with ${t.name}` : `Chat with ${scope.name}`;
    }, [scope, teammates]);

    const renderLeadMessages = () => (
        <>
            {messages.length === 0 && !streaming && (
                <div className="text-sm italic" style={{ color: 'var(--muted)' }}>
                    {activeId ? 'No messages in this chat yet.' : 'Start typing below to begin a new chat.'}
                </div>
            )}
            {messages.map((msg) => (
                <div
                    key={msg.id}
                    className={`rounded-lg px-3 py-2 max-w-[90%] sm:max-w-[80%] ${msg.role === 'user' ? 'bg-blue-900/40 self-end' : ''}`}
                    style={msg.role === 'assistant' ? { background: 'var(--smoo-dark-blue-850)', border: '1px solid var(--border)' } : {}}
                >
                    <div className="text-[11px] mb-1" style={{ color: 'var(--muted)' }}>
                        {msg.role === 'user' ? 'You' : 'Big Smooth'}
                    </div>
                    {msg.role === 'assistant' ? (
                        <ReactMarkdown
                            remarkPlugins={[remarkGfm]}
                            components={{
                                code: (props) => (
                                    <code className="px-1 py-0.5 rounded text-sm font-mono" style={{ background: '#0a1f7a', color: 'var(--smoo-green)' }}>
                                        {props.children}
                                    </code>
                                ),
                                h1: (props) => <h1 className="text-xl font-bold mb-2">{props.children}</h1>,
                                h2: (props) => <h2 className="text-lg font-semibold mb-2">{props.children}</h2>,
                                p: (props) => <p className="mb-2">{props.children}</p>,
                                table: (props) => (
                                    <div className="overflow-x-auto mb-2">
                                        <table className="min-w-full border-collapse text-sm" style={{ border: '1px solid var(--border)' }}>
                                            {props.children}
                                        </table>
                                    </div>
                                ),
                                th: (props) => (
                                    <th className="px-3 py-1.5 text-left font-semibold" style={{ border: '1px solid var(--border)', background: 'var(--smoo-dark-blue-850)' }}>
                                        {props.children}
                                    </th>
                                ),
                                td: (props) => (
                                    <td className="px-3 py-1.5" style={{ border: '1px solid var(--border)' }}>{props.children}</td>
                                ),
                            }}
                        >
                            {msg.content}
                        </ReactMarkdown>
                    ) : (
                        <div className="whitespace-pre-wrap break-words">{msg.content}</div>
                    )}
                </div>
            ))}
        </>
    );

    const renderTeammateMessages = () => {
        if (teammateMessages.length === 0) {
            return (
                <div className="text-sm italic" style={{ color: 'var(--muted)' }}>
                    No messages yet. Type below to send a message — the teammate will see it as a user-turn.
                </div>
            );
        }
        return teammateMessages.map((m) => {
            const isUser = m.from === 'user';
            const isTeammate = m.from === 'teammate';
            const isLead = m.from === 'lead';
            const label = isUser ? 'You' : isTeammate ? scope.kind === 'teammate' ? scope.name : 'Teammate' : isLead ? 'Big Smooth' : 'System';
            // Strip prefix tags from the rendered body for readability.
            const cleaned = m.content.replace(/^\s*\[[A-Z:_a-z0-9-]+\]\s*/, '');
            return (
                <div
                    key={m.id}
                    className={`rounded-lg px-3 py-2 max-w-[90%] sm:max-w-[80%] ${isUser ? 'bg-blue-900/40 self-end' : ''}`}
                    style={!isUser ? { background: 'var(--smoo-dark-blue-850)', border: '1px solid var(--border)' } : {}}
                >
                    <div className="text-[11px] mb-1 flex gap-2" style={{ color: 'var(--muted)' }}>
                        <span>{label}</span>
                        <RelativeTime iso={m.timestamp} />
                    </div>
                    <div className="whitespace-pre-wrap break-words">{cleaned}</div>
                </div>
            );
        });
    };

    const Conversation = (
        <div className={`flex flex-col min-w-0 min-h-0 ${isMobile ? 'w-full flex-1' : 'flex-1'}`}>
            <div className="flex items-center gap-3 mb-3">
                {isMobile && (
                    <button
                        onClick={() => {
                            if (scope.kind === 'teammate') setScope({ kind: 'lead' });
                            else setActiveId(null);
                        }}
                        type="button"
                        className="rounded hover:bg-white/5 cursor-pointer min-h-[36px] min-w-[36px] flex items-center justify-center"
                        aria-label="Back"
                    >
                        <ArrowLeft size={18} />
                    </button>
                )}
                {scope.kind === 'lead' && (
                    <div className="relative shrink-0" style={{ width: isMobile ? 64 : 96, height: isMobile ? 64 : 96 }}>
                        <BigSmoothFace state={streaming ? 'thinking' : 'idle'} size={isMobile ? 64 : 96} />
                    </div>
                )}
                <h1 className={`font-bold ${isMobile ? 'text-lg' : 'text-2xl'}`}>{conversationTitle}</h1>
            </div>
            {scope.kind === 'lead' && (streaming || thoughts.length > 0) && (
                <div
                    className="flex flex-wrap gap-2 mb-3 px-3 py-2 rounded-lg"
                    style={{
                        background: 'color-mix(in oklch, var(--smoo-green) 6%, transparent)',
                        border: '1px solid color-mix(in oklch, var(--smoo-green) 30%, transparent)',
                    }}
                    aria-live="polite"
                >
                    {thoughts.length === 0 && streaming && <PendingThoughtBubble />}
                    {thoughts.map((t, i) => (
                        <ThoughtBubble key={t.id} text={t.text} freshness={(i + 1) / thoughts.length} />
                    ))}
                </div>
            )}
            <div className="flex-1 overflow-auto flex flex-col gap-3 mb-3 min-h-0">
                {scope.kind === 'lead' ? renderLeadMessages() : renderTeammateMessages()}
                {streaming && scope.kind !== 'lead' && (
                    <div className="italic text-sm" style={{ color: 'var(--muted)' }}>
                        Sending…
                    </div>
                )}
                <div ref={bottomRef} />
            </div>
            <div className="relative shrink-0">
                {acMode === 'slash' && slashSuggestions.length > 0 && (
                    <div
                        className="absolute bottom-full left-0 right-0 mb-2 rounded-lg overflow-hidden z-10"
                        style={{ background: 'var(--smoo-dark-blue-850)', border: '2px solid var(--border)', maxHeight: '240px', overflowY: 'auto' }}
                    >
                        <div className="px-3 py-1.5 text-[11px] uppercase tracking-wide" style={{ color: 'var(--muted)', borderBottom: '1px solid var(--border)' }}>
                            Slash commands
                        </div>
                        {slashSuggestions.map((cmd, i) => (
                            <button
                                key={cmd.name}
                                type="button"
                                onClick={() => {
                                    setAcIndex(i);
                                    acceptAutocomplete();
                                }}
                                onMouseEnter={() => setAcIndex(i)}
                                className="w-full text-left px-3 py-2 cursor-pointer flex justify-between gap-3"
                                style={{ background: i === acIndex ? 'var(--smoo-green-alpha)' : 'transparent' }}
                            >
                                <span className="font-mono text-sm" style={{ color: 'var(--smoo-green)' }}>
                                    {cmd.name}
                                </span>
                                <span className="text-[12px] truncate" style={{ color: 'var(--muted)' }}>
                                    {cmd.description}
                                </span>
                            </button>
                        ))}
                    </div>
                )}
                {acMode === 'mention' && acResults.length > 0 && (
                    <div
                        className="absolute bottom-full left-0 right-0 mb-2 rounded-lg overflow-hidden z-10"
                        style={{ background: 'var(--smoo-dark-blue-850)', border: '2px solid var(--border)', maxHeight: '320px', overflowY: 'auto' }}
                    >
                        <div className="px-3 py-1.5 text-[11px] uppercase tracking-wide" style={{ color: 'var(--muted)', borderBottom: '1px solid var(--border)' }}>
                            @{acQuery}
                        </div>
                        {acResults.map((r, i) => (
                            <button
                                key={`${r.kind}-${r.insert}-${i}`}
                                type="button"
                                onClick={() => {
                                    setAcIndex(i);
                                    acceptAutocomplete();
                                }}
                                onMouseEnter={() => setAcIndex(i)}
                                className="w-full text-left px-3 py-2 cursor-pointer flex flex-col gap-0.5"
                                style={{ background: i === acIndex ? 'var(--smoo-green-alpha)' : 'transparent' }}
                            >
                                <div className="flex items-center gap-2">
                                    <span className="text-[10px] uppercase tracking-wide rounded px-1.5 py-0.5" style={{ background: 'var(--smoo-green-alpha)', color: 'var(--smoo-green)' }}>
                                        {r.kind}
                                    </span>
                                    <span className="text-sm font-medium truncate">{r.label}</span>
                                </div>
                                {r.detail && (
                                    <div className="text-[12px] truncate" style={{ color: 'var(--muted)' }}>
                                        {r.detail}
                                    </div>
                                )}
                            </button>
                        ))}
                    </div>
                )}
                <form
                    onSubmit={(e) => {
                        e.preventDefault();
                        if (acMode) {
                            acceptAutocomplete();
                            return;
                        }
                        send();
                    }}
                    className="flex gap-2"
                >
                    <input
                        ref={inputRef}
                        value={input}
                        onChange={(e) => {
                            setInput(e.target.value);
                            refreshAutocomplete(e.target.value);
                        }}
                        onKeyDown={(e) => {
                            if (acMode) {
                                if (e.key === 'ArrowDown') {
                                    e.preventDefault();
                                    const total = acMode === 'slash' ? slashSuggestions.length : acResults.length;
                                    setAcIndex((i) => (i + 1) % Math.max(1, total));
                                    return;
                                }
                                if (e.key === 'ArrowUp') {
                                    e.preventDefault();
                                    const total = acMode === 'slash' ? slashSuggestions.length : acResults.length;
                                    setAcIndex((i) => (i - 1 + Math.max(1, total)) % Math.max(1, total));
                                    return;
                                }
                                if (e.key === 'Tab' || (e.key === 'Enter' && !e.shiftKey)) {
                                    e.preventDefault();
                                    acceptAutocomplete();
                                    return;
                                }
                                if (e.key === 'Escape') {
                                    e.preventDefault();
                                    setAcMode(null);
                                    return;
                                }
                            }
                            if (e.key === 'Enter' && !e.shiftKey) {
                                e.preventDefault();
                                send();
                            }
                        }}
                        placeholder={
                            scope.kind === 'lead'
                                ? activeId
                                    ? 'Message Big Smooth — try /pearls or @ to search'
                                    : 'Start a new chat — try /help, /pearls, /teammates, or @ to mention'
                                : `Message ${scope.name} — / for commands, @ to search`
                        }
                        className="flex-1 min-w-0 rounded-lg px-4 py-4 outline-none focus:ring-2 transition-shadow"
                        style={{
                            background: 'var(--color-card)',
                            border: '2px solid var(--color-border)',
                            color: 'var(--color-foreground)',
                            fontSize: '16px',
                        }}
                        enterKeyHint="send"
                        autoComplete="off"
                        aria-label="Message"
                    />
                    {streaming ? (
                        <button
                            type="button"
                            onClick={stopSend}
                            className="rounded-lg px-5 sm:px-6 py-4 font-semibold flex items-center justify-center gap-2 cursor-pointer shrink-0 min-h-[56px] min-w-[56px] shadow-lg"
                            style={{ background: 'oklch(0.65 0.20 25)', color: '#020618' }}
                            aria-label="Stop"
                            title="Cancel — your input field will unlock"
                        >
                            <Square size={18} strokeWidth={3} fill="#020618" />
                            <span className="hidden sm:inline">Stop</span>
                        </button>
                    ) : (
                        <button
                            type="submit"
                            disabled={!input.trim()}
                            className="rounded-lg px-5 sm:px-6 py-4 font-semibold flex items-center justify-center gap-2 cursor-pointer disabled:opacity-40 disabled:cursor-not-allowed shrink-0 min-h-[56px] min-w-[56px] shadow-lg"
                            style={{ background: 'var(--smoo-green)', color: '#020618' }}
                            aria-label="Send"
                        >
                            <Send size={20} strokeWidth={2.5} />
                            <span className="hidden sm:inline">Send</span>
                        </button>
                    )}
                </form>
            </div>
        </div>
    );

    return (
        <div className={`flex gap-4 ${isMobile ? 'flex-col h-[calc(100dvh-88px)]' : 'flex-row h-[calc(100vh-104px)]'}`}>
            {!showConvoOnMobile && Sidebar}
            {!showListOnMobile && Conversation}
        </div>
    );
}

/// One floating one-liner from Big Smooth's inner monologue. `freshness`
/// runs 0 (oldest) → 1 (newest) so we can fade older bubbles back.
function ThoughtBubble({ text, freshness }: { text: string; freshness: number }) {
    // Newest bubble is fully opaque; oldest still very legible.
    const opacity = 0.7 + 0.3 * freshness;
    return (
        <div
            className="rounded-full whitespace-nowrap overflow-hidden text-ellipsis px-3 py-1.5 text-[13px]"
            style={{
                background: 'color-mix(in oklch, var(--smoo-green) 28%, transparent)',
                border: '1px solid color-mix(in oklch, var(--smoo-green) 70%, transparent)',
                color: 'var(--color-foreground)',
                opacity,
                maxWidth: '380px',
                animation: 'bs-thought-in 280ms ease-out both',
                fontStyle: 'italic',
                fontWeight: 500,
            }}
            title={text}
        >
            {text}
        </div>
    );
}

/// Placeholder bubble shown when streaming has started but the first
/// summarized thought hasn't landed yet. Animated dots so the user
/// sees something alive immediately.
function PendingThoughtBubble() {
    return (
        <div
            className="rounded-full px-3 py-1.5 text-[13px] flex items-center gap-1.5"
            style={{
                background: 'color-mix(in oklch, var(--smoo-green) 18%, transparent)',
                border: '1px solid color-mix(in oklch, var(--smoo-green) 55%, transparent)',
                color: 'var(--color-foreground)',
                fontStyle: 'italic',
                opacity: 0.85,
            }}
        >
            <span>Big Smooth is thinking</span>
            <span className="inline-flex gap-0.5">
                <span className="bs-dot" style={{ animationDelay: '0ms' }}>·</span>
                <span className="bs-dot" style={{ animationDelay: '150ms' }}>·</span>
                <span className="bs-dot" style={{ animationDelay: '300ms' }}>·</span>
            </span>
        </div>
    );
}

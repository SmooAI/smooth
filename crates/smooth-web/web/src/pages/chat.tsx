import { useCallback, useEffect, useRef, useState } from 'react';
import { Plus, Send, Trash2 } from 'lucide-react';
import ReactMarkdown from 'react-markdown';
import remarkGfm from 'remark-gfm';
import { api } from '../api';

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
    const bottomRef = useRef<HTMLDivElement>(null);

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

    useEffect(() => {
        refreshSessions();
    }, [refreshSessions]);

    // Load messages when active session changes.
    useEffect(() => {
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
    }, [activeId]);

    useEffect(() => {
        bottomRef.current?.scrollIntoView({ behavior: 'smooth' });
    }, [messages.length, streaming]);

    const newChat = useCallback(async () => {
        try {
            const r = await api<{ data: Session }>('/api/chat/sessions', {
                method: 'POST',
                body: JSON.stringify({}),
            });
            setSessions((prev) => [r.data, ...prev]);
            setActiveId(r.data.id);
            setMessages([]);
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

    const send = useCallback(async () => {
        const content = input.trim();
        if (!content || streaming) return;

        // If there's no active session, create one first.
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

        try {
            const resp = await fetch(`/api/chat/sessions/${sessionId}/messages`, {
                method: 'POST',
                headers: { 'Content-Type': 'application/json' },
                body: JSON.stringify({ content }),
            });
            const json = await resp.json();
            const assistantMsg: Msg = {
                id: json.data?.id ?? `resp-${Date.now()}`,
                role: 'assistant',
                content: json.data?.content ?? 'No response',
            };
            setMessages((prev) => [...prev, assistantMsg]);
            // Refresh the session so the sidebar title picks up the
            // first-message rename and message count bump.
            refreshSessions();
        } catch (e) {
            setMessages((prev) => [...prev, { id: `err-${Date.now()}`, role: 'assistant', content: `Error: ${(e as Error).message}` }]);
        } finally {
            setStreaming(false);
        }
    }, [input, streaming, activeId, refreshSessions]);

    return (
        // Layout chrome = 56px header (h-14) + 48px main padding (p-6 top+bottom)
        <div className="flex h-[calc(100vh-104px)] gap-4">
            {/* Sessions sidebar */}
            <aside
                className="w-64 shrink-0 flex flex-col rounded-lg"
                style={{ background: 'var(--smoo-dark-blue-850)', border: '1px solid var(--border)' }}
            >
                <div className="p-3 flex items-center justify-between border-b" style={{ borderColor: 'var(--border)' }}>
                    <h2 className="text-sm font-semibold">Chats</h2>
                    <button
                        onClick={() => newChat()}
                        type="button"
                        className="p-1.5 rounded hover:bg-white/5 cursor-pointer"
                        title="New chat"
                    >
                        <Plus size={14} />
                    </button>
                </div>
                <div className="flex-1 overflow-auto">
                    {loadingSessions && (
                        <div className="p-3 text-xs" style={{ color: 'var(--muted)' }}>
                            Loading…
                        </div>
                    )}
                    {!loadingSessions && sessions.length === 0 && (
                        <div className="p-3 text-xs" style={{ color: 'var(--muted)' }}>
                            No chats yet. Click + to start one.
                        </div>
                    )}
                    {sessions.map((s) => (
                        <div
                            key={s.id}
                            onClick={() => setActiveId(s.id)}
                            className={`px-3 py-2 cursor-pointer border-b group ${activeId === s.id ? '' : 'hover:bg-white/5'}`}
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
                                    className="opacity-0 group-hover:opacity-100 p-1 rounded hover:bg-white/10 cursor-pointer shrink-0"
                                    title="Delete chat"
                                >
                                    <Trash2 size={12} />
                                </button>
                            </div>
                            <div className="text-[11px] flex items-center gap-2 mt-0.5" style={{ color: 'var(--muted)' }}>
                                <span>
                                    {s.message_count} msg
                                    {s.message_count === 1 ? '' : 's'}
                                </span>
                                <span>·</span>
                                <RelativeTime iso={s.started_at} />
                            </div>
                        </div>
                    ))}
                </div>
            </aside>

            {/* Conversation */}
            <div className="flex-1 flex flex-col min-w-0">
                <h1 className="text-2xl font-bold mb-4">Chat with Big Smooth</h1>
                <div className="flex-1 overflow-auto flex flex-col gap-3 mb-4">
                    {messages.length === 0 && !streaming && (
                        <div className="text-sm italic" style={{ color: 'var(--muted)' }}>
                            {activeId
                                ? 'No messages in this chat yet.'
                                : 'Start typing to begin a new chat, or pick one from the sidebar.'}
                        </div>
                    )}
                    {messages.map((msg) => (
                        <div
                            key={msg.id}
                            className={`rounded-lg px-3 py-2 max-w-[80%] ${msg.role === 'user' ? 'bg-blue-900/40 self-end' : ''}`}
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
                                            <td className="px-3 py-1.5" style={{ border: '1px solid var(--border)' }}>
                                                {props.children}
                                            </td>
                                        ),
                                    }}
                                >
                                    {msg.content}
                                </ReactMarkdown>
                            ) : (
                                <div>{msg.content}</div>
                            )}
                        </div>
                    ))}
                    {streaming && (
                        <div className="italic text-sm" style={{ color: 'var(--muted)' }}>
                            Thinking...
                        </div>
                    )}
                    <div ref={bottomRef} />
                </div>
                <div className="flex gap-2">
                    <input
                        value={input}
                        onChange={(e) => setInput(e.target.value)}
                        onKeyDown={(e) => {
                            if (e.key === 'Enter' && !e.shiftKey) {
                                e.preventDefault();
                                send();
                            }
                        }}
                        placeholder={activeId ? 'Message Big Smooth...' : 'Start a new chat...'}
                        className="flex-1 rounded-lg px-4 py-3 text-sm outline-none"
                        style={{ background: 'var(--smoo-dark-blue-850)', border: '1px solid var(--border)', color: '#f8fafc' }}
                    />
                    <button
                        onClick={() => send()}
                        type="button"
                        className="rounded-lg px-6 py-3 font-semibold flex items-center gap-2 cursor-pointer"
                        style={{ background: 'var(--smoo-green)', color: '#020618' }}
                    >
                        <Send size={16} /> Send
                    </button>
                </div>
            </div>
        </div>
    );
}

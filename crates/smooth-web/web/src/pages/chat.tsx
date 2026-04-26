import { useCallback, useEffect, useRef, useState } from 'react';
import { ArrowLeft, Plus, Send, Trash2 } from 'lucide-react';
import ReactMarkdown from 'react-markdown';
import remarkGfm from 'remark-gfm';
import { api } from '../api';
import { useIsMobile } from '../hooks/use-mobile';

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
    const isMobile = useIsMobile();

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
            refreshSessions();
        } catch (e) {
            setMessages((prev) => [...prev, { id: `err-${Date.now()}`, role: 'assistant', content: `Error: ${(e as Error).message}` }]);
        } finally {
            setStreaming(false);
        }
    }, [input, streaming, activeId, refreshSessions]);

    // Mobile: show one pane at a time. List when no active chat, conversation when one is selected.
    const showListOnMobile = isMobile && !activeId;
    const showConvoOnMobile = isMobile && !!activeId;

    const Sessions = (
        <aside
            className={`flex flex-col rounded-lg ${isMobile ? 'w-full flex-1 min-h-0' : 'w-64 shrink-0'}`}
            style={{ background: 'var(--smoo-dark-blue-850)', border: '1px solid var(--border)' }}
        >
            <div className="p-3 flex items-center justify-between border-b" style={{ borderColor: 'var(--border)' }}>
                <h2 className="text-sm font-semibold">Chats</h2>
                <button
                    onClick={() => newChat()}
                    type="button"
                    className="rounded hover:bg-white/5 cursor-pointer min-h-[36px] min-w-[36px] flex items-center justify-center"
                    title="New chat"
                    aria-label="New chat"
                >
                    <Plus size={16} />
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
    );

    const Conversation = (
        <div className={`flex flex-col min-w-0 min-h-0 ${isMobile ? 'w-full flex-1' : 'flex-1'}`}>
            <div className="flex items-center gap-2 mb-3">
                {isMobile && (
                    <button
                        onClick={() => setActiveId(null)}
                        type="button"
                        className="rounded hover:bg-white/5 cursor-pointer min-h-[36px] min-w-[36px] flex items-center justify-center"
                        aria-label="Back to chats"
                    >
                        <ArrowLeft size={18} />
                    </button>
                )}
                <h1 className={`font-bold ${isMobile ? 'text-lg' : 'text-2xl'}`}>Chat with Big Smooth</h1>
            </div>
            <div className="flex-1 overflow-auto flex flex-col gap-3 mb-3 min-h-0">
                {messages.length === 0 && !streaming && (
                    <div className="text-sm italic" style={{ color: 'var(--muted)' }}>
                        {activeId
                            ? 'No messages in this chat yet.'
                            : 'Start typing below to begin a new chat.'}
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
                                        <td className="px-3 py-1.5" style={{ border: '1px solid var(--border)' }}>
                                            {props.children}
                                        </td>
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
                {streaming && (
                    <div className="italic text-sm" style={{ color: 'var(--muted)' }}>
                        Thinking...
                    </div>
                )}
                <div ref={bottomRef} />
            </div>
            <form
                onSubmit={(e) => {
                    e.preventDefault();
                    send();
                }}
                className="flex gap-2 shrink-0"
            >
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
                    className="flex-1 min-w-0 rounded-lg px-3 py-3 outline-none"
                    style={{ background: 'var(--smoo-dark-blue-850)', border: '1px solid var(--border)', color: '#f8fafc', fontSize: '16px' }}
                    enterKeyHint="send"
                    autoComplete="off"
                    aria-label="Message"
                />
                <button
                    type="submit"
                    disabled={!input.trim() || streaming}
                    className="rounded-lg px-4 sm:px-6 py-3 font-semibold flex items-center justify-center gap-2 cursor-pointer disabled:opacity-50 disabled:cursor-not-allowed shrink-0 min-h-[48px] min-w-[48px]"
                    style={{ background: 'var(--smoo-green)', color: '#020618' }}
                    aria-label="Send"
                >
                    <Send size={18} />
                    <span className="hidden sm:inline">Send</span>
                </button>
            </form>
        </div>
    );

    // Use 100dvh on mobile to account for browser chrome (URL bar, etc.) — 100vh is sometimes
    // the wrong height on iOS. The 88px subtracts header (56px) + page padding (16+16=32px on mobile).
    return (
        <div className={`flex gap-4 ${isMobile ? 'flex-col h-[calc(100dvh-88px)]' : 'flex-row h-[calc(100vh-104px)]'}`}>
            {!showConvoOnMobile && Sessions}
            {!showListOnMobile && Conversation}
        </div>
    );
}

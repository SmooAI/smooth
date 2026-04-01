import { useCallback, useRef, useState } from 'react';
import { Send } from 'lucide-react';
import ReactMarkdown from 'react-markdown';
import remarkGfm from 'remark-gfm';

interface Msg { role: string; content: string }

export function ChatPage() {
    const [messages, setMessages] = useState<Msg[]>([{ role: 'assistant', content: 'Welcome to Smooth. How can I help?' }]);
    const [input, setInput] = useState('');
    const [streaming, setStreaming] = useState(false);
    const bottomRef = useRef<HTMLDivElement>(null);

    const send = useCallback(async () => {
        const content = input.trim();
        if (!content || streaming) return;
        setInput('');
        setMessages((prev) => [...prev, { role: 'user', content }]);
        setStreaming(true);

        try {
            const resp = await fetch('/api/chat', {
                method: 'POST',
                headers: { 'Content-Type': 'application/json' },
                body: JSON.stringify({ content }),
            });
            const json = await resp.json();
            setMessages((prev) => [...prev, { role: 'assistant', content: json.data || 'No response' }]);
        } catch (e) {
            setMessages((prev) => [...prev, { role: 'assistant', content: `Error: ${(e as Error).message}` }]);
        } finally {
            setStreaming(false);
            bottomRef.current?.scrollIntoView({ behavior: 'smooth' });
        }
    }, [input, streaming]);

    return (
        <div className="flex flex-col h-[calc(100vh-48px)]">
            <h1 className="text-2xl font-bold mb-4">Chat with Leader</h1>
            <div className="flex-1 overflow-auto flex flex-col gap-3 mb-4">
                {messages.map((msg, i) => (
                    <div key={i} className={`rounded-lg px-3 py-2 max-w-[80%] ${msg.role === 'user' ? 'bg-blue-900/40 self-end' : ''}`}
                        style={msg.role === 'assistant' ? { background: 'var(--smoo-dark-blue-850)', border: '1px solid var(--border)' } : {}}>
                        <div className="text-[11px] mb-1" style={{ color: 'var(--muted)' }}>{msg.role === 'user' ? 'You' : 'Smooth'}</div>
                        {msg.role === 'assistant' ? (
                            <ReactMarkdown remarkPlugins={[remarkGfm]} components={{
                                code: (props) => <code className="px-1 py-0.5 rounded text-sm font-mono" style={{ background: '#0a1f7a', color: 'var(--smoo-green)' }}>{props.children}</code>,
                                h1: (props) => <h1 className="text-xl font-bold mb-2">{props.children}</h1>,
                                h2: (props) => <h2 className="text-lg font-semibold mb-2">{props.children}</h2>,
                                p: (props) => <p className="mb-2">{props.children}</p>,
                                table: (props) => <div className="overflow-x-auto mb-2"><table className="min-w-full border-collapse text-sm" style={{ border: '1px solid var(--border)' }}>{props.children}</table></div>,
                                th: (props) => <th className="px-3 py-1.5 text-left font-semibold" style={{ border: '1px solid var(--border)', background: 'var(--smoo-dark-blue-850)' }}>{props.children}</th>,
                                td: (props) => <td className="px-3 py-1.5" style={{ border: '1px solid var(--border)' }}>{props.children}</td>,
                            }}>{msg.content}</ReactMarkdown>
                        ) : (
                            <div>{msg.content}</div>
                        )}
                    </div>
                ))}
                {streaming && <div className="italic text-sm" style={{ color: 'var(--muted)' }}>Thinking...</div>}
                <div ref={bottomRef} />
            </div>
            <div className="flex gap-2">
                <input value={input} onChange={(e) => setInput(e.target.value)}
                    onKeyDown={(e) => { if (e.key === 'Enter' && !e.shiftKey) { e.preventDefault(); send(); } }}
                    placeholder="Message the leader..."
                    className="flex-1 rounded-lg px-4 py-3 text-sm outline-none"
                    style={{ background: 'var(--smoo-dark-blue-850)', border: '1px solid var(--border)', color: '#f8fafc' }} />
                <button onClick={() => send()} type="button"
                    className="rounded-lg px-6 py-3 font-semibold flex items-center gap-2 cursor-pointer"
                    style={{ background: 'var(--smoo-green)', color: '#020618' }}>
                    <Send size={16} /> Send
                </button>
            </div>
        </div>
    );
}

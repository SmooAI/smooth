'use client';

import { useCallback, useRef, useState } from 'react';

interface ChatMessage {
    role: 'user' | 'assistant' | 'tool' | 'reasoning';
    content: string;
}

export default function ChatPage() {
    const [messages, setMessages] = useState<ChatMessage[]>([{ role: 'assistant', content: 'Welcome to Smooth. How can I help?' }]);
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
            const response = await fetch('/api/chat', {
                method: 'POST',
                headers: { 'Content-Type': 'application/json' },
                body: JSON.stringify({ content }),
            });

            const reader = response.body?.getReader();
            if (!reader) return;

            const decoder = new TextDecoder();
            let assistantContent = '';

            while (true) {
                const { done, value } = await reader.read();
                if (done) break;

                const text = decoder.decode(value);
                for (const line of text.split('\n')) {
                    if (!line.startsWith('data: ')) continue;
                    try {
                        const event = JSON.parse(line.slice(6));
                        if (event.type === 'text') assistantContent += event.content;
                        if (event.type === 'reasoning') setMessages((prev) => [...prev, { role: 'reasoning', content: event.content }]);
                        if (event.type === 'tool_call') setMessages((prev) => [...prev, { role: 'tool', content: event.content }]);
                    } catch { /* ignore */ }
                }
            }

            if (assistantContent) {
                setMessages((prev) => [...prev, { role: 'assistant', content: assistantContent }]);
            }
        } catch (error) {
            setMessages((prev) => [...prev, { role: 'assistant', content: `Error: ${(error as Error).message}` }]);
        } finally {
            setStreaming(false);
            bottomRef.current?.scrollIntoView({ behavior: 'smooth' });
        }
    }, [input, streaming]);

    return (
        <div style={{ display: 'flex', flexDirection: 'column', height: 'calc(100vh - 48px)' }}>
            <h1 style={{ fontSize: 24, fontWeight: 700, marginBottom: 16 }}>Chat with Leader</h1>

            {/* Messages */}
            <div style={{ flex: 1, overflow: 'auto', display: 'flex', flexDirection: 'column', gap: 12, marginBottom: 16 }}>
                {messages.map((msg, i) => (
                    <MessageBubble key={i} message={msg} />
                ))}
                {streaming && <div style={{ color: '#525252', fontStyle: 'italic' }}>Thinking...</div>}
                <div ref={bottomRef} />
            </div>

            {/* Input */}
            <div style={{ display: 'flex', gap: 8 }}>
                <input
                    value={input}
                    onChange={(e) => setInput(e.target.value)}
                    onKeyDown={(e) => e.key === 'Enter' && send()}
                    placeholder="Message the leader... (@ for context search)"
                    style={{
                        flex: 1,
                        background: '#171717',
                        border: '1px solid #262626',
                        borderRadius: 8,
                        padding: '12px 16px',
                        color: '#e5e5e5',
                        fontSize: 14,
                        outline: 'none',
                    }}
                />
                <button
                    onClick={send}
                    disabled={streaming || !input.trim()}
                    style={{
                        background: '#06b6d4',
                        color: '#000',
                        border: 'none',
                        borderRadius: 8,
                        padding: '12px 24px',
                        fontWeight: 600,
                        cursor: streaming ? 'not-allowed' : 'pointer',
                        opacity: streaming || !input.trim() ? 0.5 : 1,
                    }}
                >
                    Send
                </button>
            </div>
        </div>
    );
}

function MessageBubble({ message }: { message: ChatMessage }) {
    const styles: Record<string, React.CSSProperties> = {
        user: { background: '#1e3a5f', borderRadius: 8, padding: '8px 12px', alignSelf: 'flex-end', maxWidth: '80%' },
        assistant: { background: '#171717', border: '1px solid #262626', borderRadius: 8, padding: '8px 12px', maxWidth: '80%' },
        tool: { background: '#1a1a2e', border: '1px solid #312e81', borderRadius: 8, padding: '8px 12px', fontSize: 13, color: '#a5b4fc', maxWidth: '80%' },
        reasoning: { fontStyle: 'italic', color: '#525252', padding: '4px 12px', fontSize: 13 },
    };

    const labels: Record<string, string> = {
        user: 'You',
        assistant: 'Smooth',
        tool: 'Tool',
        reasoning: '',
    };

    return (
        <div style={styles[message.role]}>
            {labels[message.role] && <div style={{ fontSize: 11, color: '#737373', marginBottom: 4 }}>{labels[message.role]}</div>}
            <div style={{ whiteSpace: 'pre-wrap' }}>{message.content}</div>
        </div>
    );
}

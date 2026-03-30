'use client';

import { Send } from 'lucide-react';
import { useCallback, useRef, useState } from 'react';

import { AtAutocomplete, insertAtResult, type SearchResult } from '@/components/at-autocomplete';
import Markdown from '@/components/markdown';

interface ChatMessage {
    role: 'user' | 'assistant' | 'tool' | 'reasoning';
    content: string;
}

export default function ChatPage() {
    const [messages, setMessages] = useState<ChatMessage[]>([{ role: 'assistant', content: 'Welcome to Smooth. How can I help?' }]);
    const [input, setInput] = useState('');
    const [streaming, setStreaming] = useState(false);
    const [cursorPos, setCursorPos] = useState(0);
    const bottomRef = useRef<HTMLDivElement>(null);
    const inputRef = useRef<HTMLInputElement>(null);

    const handleAtSelect = useCallback(
        (result: SearchResult) => {
            const { newInput, newCursor } = insertAtResult(input, cursorPos, result);
            setInput(newInput);
            setCursorPos(newCursor);
            setTimeout(() => {
                if (inputRef.current) {
                    inputRef.current.focus();
                    inputRef.current.setSelectionRange(newCursor, newCursor);
                }
            }, 0);
        },
        [input, cursorPos],
    );

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
            let assistantAdded = false;

            while (true) {
                const { done, value } = await reader.read();
                if (done) break;

                const text = decoder.decode(value);
                for (const line of text.split('\n')) {
                    if (!line.startsWith('data: ')) continue;
                    try {
                        const event = JSON.parse(line.slice(6));
                        if (event.type === 'text') {
                            assistantContent += event.content;
                            if (!assistantAdded) {
                                setMessages((prev) => [...prev, { role: 'assistant', content: assistantContent }]);
                                assistantAdded = true;
                            } else {
                                // Update the last message in place for streaming effect
                                setMessages((prev) => [...prev.slice(0, -1), { role: 'assistant', content: assistantContent }]);
                            }
                        }
                        if (event.type === 'reasoning') setMessages((prev) => [...prev, { role: 'reasoning', content: event.content }]);
                        if (event.type === 'tool_call') setMessages((prev) => [...prev, { role: 'tool', content: event.content }]);
                    } catch {
                        /* ignore */
                    }
                }
            }
        } catch (error) {
            setMessages((prev) => [...prev, { role: 'assistant', content: `Error: ${(error as Error).message}` }]);
        } finally {
            setStreaming(false);
            bottomRef.current?.scrollIntoView({ behavior: 'smooth' });
        }
    }, [input, streaming]);

    return (
        <div className="flex flex-col h-[calc(100vh-48px)]">
            <h1 className="text-2xl font-bold mb-4">Chat with Leader</h1>

            {/* Messages */}
            <div className="flex-1 overflow-auto flex flex-col gap-3 mb-4">
                {messages.map((msg, i) => (
                    <MessageBubble key={i} message={msg} />
                ))}
                {streaming && <div className="text-neutral-600 italic text-sm">Thinking...</div>}
                <div ref={bottomRef} />
            </div>

            {/* Input with @ autocomplete */}
            <div className="flex gap-2 relative">
                <AtAutocomplete input={input} cursorPosition={cursorPos} onSelect={handleAtSelect} />
                <input
                    ref={inputRef}
                    value={input}
                    onChange={(e) => {
                        setInput(e.target.value);
                        setCursorPos(e.target.selectionStart ?? e.target.value.length);
                    }}
                    onKeyUp={(e) => setCursorPos((e.target as HTMLInputElement).selectionStart ?? 0)}
                    onClick={(e) => setCursorPos((e.target as HTMLInputElement).selectionStart ?? 0)}
                    onKeyDown={(e) => {
                        if (e.key === 'Enter' && !e.shiftKey) {
                            e.preventDefault();
                            send();
                        }
                    }}
                    placeholder="Message the leader... (@ for context search)"
                    className="flex-1 bg-neutral-900 border border-neutral-800 rounded-lg px-4 py-3 text-neutral-100 text-sm outline-none focus:border-cyan-600 transition-colors placeholder:text-neutral-600"
                />
                <button
                    onClick={() => send()}
                    type="button"
                    className="bg-cyan-500 text-black rounded-lg px-6 py-3 font-semibold flex items-center gap-2 cursor-pointer hover:bg-cyan-400"
                >
                    <Send size={16} />
                    Send
                </button>
            </div>
        </div>
    );
}

const bubbleClasses: Record<string, string> = {
    user: 'bg-blue-900/40 rounded-lg px-3 py-2 self-end max-w-[80%]',
    assistant: 'bg-neutral-900 border border-neutral-800 rounded-lg px-3 py-2 max-w-[80%]',
    tool: 'bg-indigo-950/40 border border-indigo-900/50 rounded-lg px-3 py-2 text-sm text-indigo-300 max-w-[80%]',
    reasoning: 'italic text-neutral-600 px-3 py-1 text-sm',
};

const roleLabels: Record<string, string> = {
    user: 'You',
    assistant: 'Smooth',
    tool: 'Tool',
    reasoning: '',
};

function MessageBubble({ message }: { message: ChatMessage }) {
    return (
        <div className={bubbleClasses[message.role]}>
            {roleLabels[message.role] && <div className="text-[11px] text-neutral-500 mb-1">{roleLabels[message.role]}</div>}
            {message.role === 'assistant' ? <Markdown>{message.content}</Markdown> : <div className="whitespace-pre-wrap">{message.content}</div>}
        </div>
    );
}

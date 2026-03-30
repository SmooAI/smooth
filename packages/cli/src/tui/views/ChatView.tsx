import { Box, Text, useInput } from 'ink';
import React, { useCallback, useEffect, useState } from 'react';

import type { LeaderClient } from '../../client/leader-client.js';

interface ChatMessage {
    role: 'user' | 'assistant' | 'tool' | 'reasoning';
    content: string;
}

interface Attachment {
    type: 'file' | 'bead' | 'worker' | 'artifact';
    reference: string;
    display: string;
}

interface Props {
    client: LeaderClient;
}

export function ChatView({ client }: Props) {
    const [messages, setMessages] = useState<ChatMessage[]>([{ role: 'assistant', content: 'Welcome to Smooth. How can I help?' }]);
    const [input, setInput] = useState('');
    const [focused, setFocused] = useState(true);
    const [streaming, setStreaming] = useState(false);
    const [attachments, setAttachments] = useState<Attachment[]>([]);
    const [showAtSearch, setShowAtSearch] = useState(false);
    const [atQuery, setAtQuery] = useState('');
    const [atResults, setAtResults] = useState<Attachment[]>([]);
    const [atSelected, setAtSelected] = useState(0);

    // Handle keyboard input
    useInput((ch, key) => {
        if (!focused) {
            if (key.return) setFocused(true);
            return;
        }

        if (key.escape) {
            if (showAtSearch) {
                setShowAtSearch(false);
                setAtQuery('');
                setAtResults([]);
            } else {
                setFocused(false);
            }
            return;
        }

        // @ search mode
        if (showAtSearch) {
            if (key.return && atResults.length > 0) {
                // Select result
                const selected = atResults[atSelected];
                setAttachments((prev) => [...prev, selected]);
                setShowAtSearch(false);
                setAtQuery('');
                setAtResults([]);
                setAtSelected(0);
                return;
            }
            if (key.upArrow) {
                setAtSelected((s) => Math.max(0, s - 1));
                return;
            }
            if (key.downArrow) {
                setAtSelected((s) => Math.min(atResults.length - 1, s + 1));
                return;
            }
            if (key.backspace || key.delete) {
                if (atQuery.length === 0) {
                    setShowAtSearch(false);
                    return;
                }
                setAtQuery((q) => q.slice(0, -1));
                return;
            }
            if (ch && !key.ctrl) {
                setAtQuery((q) => q + ch);
                return;
            }
            return;
        }

        // Normal input mode
        if (ch === '@') {
            setShowAtSearch(true);
            setAtQuery('');
            setAtSelected(0);
            return;
        }

        if (key.return && input.trim()) {
            sendMessage();
            return;
        }

        if (key.backspace || key.delete) {
            setInput((i) => i.slice(0, -1));
            return;
        }

        if (ch && !key.ctrl) {
            setInput((i) => i + ch);
        }
    });

    // Search when @ query changes
    useEffect(() => {
        if (!showAtSearch || !atQuery) {
            setAtResults([]);
            return;
        }

        // Search beads, files, workers
        const results: Attachment[] = [];
        const q = atQuery.toLowerCase();

        // Simulate search results — in production this queries the leader API
        if (q.length > 0) {
            results.push(
                { type: 'bead', reference: `SMOOTH-${q}`, display: `bead: SMOOTH-${q}` },
                { type: 'file', reference: `src/${q}`, display: `file: src/${q}` },
                { type: 'worker', reference: `operator-${q}`, display: `operator: operator-${q}` },
            );
        }

        // Also search via leader API
        client
            .searchBeads(atQuery)
            .then((r) => {
                const beadResults: Attachment[] = (r.data as any[]).slice(0, 5).map((b: any) => ({
                    type: 'bead' as const,
                    reference: b.id,
                    display: `bead: ${b.id} — ${b.title}`,
                }));
                setAtResults([...beadResults, ...results].slice(0, 8));
            })
            .catch(() => setAtResults(results.slice(0, 8)));
    }, [atQuery, showAtSearch, client]);

    const sendMessage = useCallback(async () => {
        const content = input.trim();
        if (!content || streaming) return;

        setInput('');
        setMessages((prev) => [...prev, { role: 'user', content }]);
        setStreaming(true);

        try {
            const response = await client.chat(
                content,
                attachments.map((a) => ({ type: a.type, reference: a.reference })),
            );
            setAttachments([]);

            // Read SSE stream
            const reader = response.body?.getReader();
            if (!reader) return;

            const decoder = new TextDecoder();
            let assistantContent = '';

            while (true) {
                const { done, value } = await reader.read();
                if (done) break;

                const text = decoder.decode(value);
                const lines = text.split('\n');

                for (const line of lines) {
                    if (!line.startsWith('data: ')) continue;
                    try {
                        const event = JSON.parse(line.slice(6));
                        if (event.type === 'text') {
                            assistantContent += event.content;
                        } else if (event.type === 'reasoning') {
                            setMessages((prev) => [...prev, { role: 'reasoning', content: event.content }]);
                        } else if (event.type === 'tool_call') {
                            setMessages((prev) => [...prev, { role: 'tool', content: event.content }]);
                        }
                    } catch {
                        // ignore parse errors
                    }
                }
            }

            if (assistantContent) {
                setMessages((prev) => [...prev, { role: 'assistant', content: assistantContent }]);
            }
        } catch (error) {
            setMessages((prev) => [...prev, { role: 'assistant', content: `Error: ${(error as Error).message}` }]);
        } finally {
            setStreaming(false);
        }
    }, [input, attachments, streaming, client]);

    return (
        <Box flexDirection="column" flexGrow={1}>
            {/* Message history */}
            <Box flexDirection="column" flexGrow={1} overflow="hidden">
                {messages.slice(-20).map((msg, i) => (
                    <MessageBubble key={i} message={msg} />
                ))}
                {streaming && (
                    <Text dimColor italic>
                        Thinking...
                    </Text>
                )}
            </Box>

            {/* @ search dropdown */}
            {showAtSearch && (
                <Box flexDirection="column" borderStyle="single" borderColor="yellow" paddingX={1}>
                    <Text color="yellow">@ {atQuery}</Text>
                    {atResults.map((r, i) => (
                        <Text key={i} color={i === atSelected ? 'cyan' : undefined} bold={i === atSelected}>
                            {i === atSelected ? '> ' : '  '}
                            {r.display}
                        </Text>
                    ))}
                    {atResults.length === 0 && atQuery && <Text dimColor>No results</Text>}
                </Box>
            )}

            {/* Attachments bar */}
            {attachments.length > 0 && (
                <Box gap={1} paddingX={1}>
                    <Text dimColor>Attached:</Text>
                    {attachments.map((a, i) => (
                        <Text key={i} color="cyan">
                            @{a.reference}
                        </Text>
                    ))}
                </Box>
            )}

            {/* Input */}
            <Box borderStyle="single" borderColor={focused ? 'cyan' : 'gray'} paddingX={1}>
                <Text color={focused ? 'cyan' : 'gray'}>&gt; </Text>
                <Text>{input}</Text>
                {focused && <Text color="cyan">|</Text>}
                {!focused && <Text dimColor>Press Enter to focus</Text>}
            </Box>
        </Box>
    );
}

function MessageBubble({ message }: { message: ChatMessage }) {
    switch (message.role) {
        case 'user':
            return (
                <Box paddingLeft={2}>
                    <Text color="green" bold>
                        You:{' '}
                    </Text>
                    <Text>{message.content}</Text>
                </Box>
            );
        case 'assistant':
            return (
                <Box paddingLeft={2}>
                    <Text color="cyan" bold>
                        Smooth:{' '}
                    </Text>
                    <Text>{message.content}</Text>
                </Box>
            );
        case 'tool':
            return (
                <Box paddingLeft={4}>
                    <Text color="yellow" dimColor>
                        [tool] {message.content}
                    </Text>
                </Box>
            );
        case 'reasoning':
            return (
                <Box paddingLeft={4}>
                    <Text italic dimColor>
                        {message.content}
                    </Text>
                </Box>
            );
    }
}

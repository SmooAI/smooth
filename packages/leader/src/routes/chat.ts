import { Hono } from 'hono';
import { streamSSE } from 'hono/streaming';
import { existsSync, readFileSync } from 'node:fs';
import { homedir } from 'node:os';
import { join } from 'node:path';

import { createAuditLogger } from '@smooai/smooth-shared/audit-log';
import { ChatMessageSchema } from '@smooai/smooth-shared/schemas';

export const chatRoutes = new Hono();

const audit = createAuditLogger('leader');

/** Get OpenCode Zen API key from opencode's auth store */
function getZenApiKey(): string | null {
    const authPath = join(homedir(), '.local', 'share', 'opencode', 'auth.json');
    if (!existsSync(authPath)) return null;
    try {
        const auth = JSON.parse(readFileSync(authPath, 'utf8'));
        return auth.opencode?.key ?? null;
    } catch {
        return null;
    }
}

/** Send a chat message — calls OpenCode Zen API directly (OpenAI-compatible) */
chatRoutes.post('/', async (c) => {
    const body = await c.req.json();
    const parsed = ChatMessageSchema.parse(body);

    const apiKey = getZenApiKey();
    if (!apiKey) {
        return streamSSE(c, async (stream) => {
            await stream.writeSSE({
                event: 'text',
                data: JSON.stringify({ type: 'text', content: 'No LLM provider configured. Run: th auth login opencode-zen' }),
            });
            await stream.writeSSE({ event: 'done', data: JSON.stringify({ type: 'done', content: '' }) });
        });
    }

    audit.promptSent('chat', parsed.content);
    const start = Date.now();

    return streamSSE(c, async (stream) => {
        try {
            // OpenCode Zen API: https://opencode.ai/zen/v1 (OpenAI-compatible)
            const response = await fetch('https://opencode.ai/zen/v1/chat/completions', {
                method: 'POST',
                headers: {
                    Authorization: `Bearer ${apiKey}`,
                    'Content-Type': 'application/json',
                },
                body: JSON.stringify({
                    model: 'claude-sonnet-4-6',
                    stream: true,
                    messages: [
                        {
                            role: 'system',
                            content: `You are Smooth, an AI agent orchestration leader. You help users manage projects, assign work to Smooth Operators (AI agents in sandboxes), review work, and coordinate tasks.

Available commands: th run <bead-id>, th operators, th pause/steer/cancel <bead-id>, th auth status, th status`,
                        },
                        { role: 'user', content: parsed.content },
                    ],
                }),
            });

            if (!response.ok) {
                const errBody = await response.text().catch(() => '');
                await stream.writeSSE({
                    event: 'text',
                    data: JSON.stringify({ type: 'text', content: `LLM error (${response.status}): ${errBody.slice(0, 200)}` }),
                });
                await stream.writeSSE({ event: 'done', data: JSON.stringify({ type: 'done', content: '' }) });
                return;
            }

            const reader = response.body?.getReader();
            if (!reader) {
                await stream.writeSSE({ event: 'text', data: JSON.stringify({ type: 'text', content: 'No response stream.' }) });
                await stream.writeSSE({ event: 'done', data: JSON.stringify({ type: 'done', content: '' }) });
                return;
            }

            const decoder = new TextDecoder();
            let fullContent = '';

            while (true) {
                const { done, value } = await reader.read();
                if (done) break;

                for (const line of decoder.decode(value).split('\n')) {
                    if (!line.startsWith('data: ')) continue;
                    const data = line.slice(6);
                    if (data === '[DONE]') break;

                    try {
                        const delta = JSON.parse(data).choices?.[0]?.delta?.content;
                        if (delta) {
                            fullContent += delta;
                            await stream.writeSSE({ event: 'text', data: JSON.stringify({ type: 'text', content: delta }) });
                        }
                    } catch {
                        /* skip */
                    }
                }
            }

            audit.promptReceived('chat', fullContent.slice(0, 200), Date.now() - start);
            await stream.writeSSE({ event: 'done', data: JSON.stringify({ type: 'done', content: '' }) });
        } catch (error) {
            await stream.writeSSE({
                event: 'text',
                data: JSON.stringify({ type: 'text', content: `Error: ${(error as Error).message}` }),
            });
            await stream.writeSSE({ event: 'done', data: JSON.stringify({ type: 'done', content: '' }) });
        }
    });
});

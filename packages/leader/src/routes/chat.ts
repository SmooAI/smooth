import { Hono } from 'hono';
import { streamSSE } from 'hono/streaming';
import { existsSync, readFileSync } from 'node:fs';
import { homedir } from 'node:os';
import { join } from 'node:path';

import { ChatMessageSchema } from '@smooai/smooth-shared/schemas';
import { createAuditLogger } from '@smooai/smooth-shared/audit-log';

export const chatRoutes = new Hono();

const audit = createAuditLogger('leader');

/** Resolve the OpenCode Zen API key and model */
function getOpenCodeAuth(): { apiKey: string; model: string; baseUrl: string } | null {
    // Check opencode's own auth store
    const authPath = join(homedir(), '.local', 'share', 'opencode', 'auth.json');
    if (existsSync(authPath)) {
        try {
            const auth = JSON.parse(readFileSync(authPath, 'utf8'));
            if (auth.opencode?.key) {
                return {
                    apiKey: auth.opencode.key,
                    model: 'opencode/claude-sonnet-4-6',
                    baseUrl: 'https://opencode.ai/api/v1',
                };
            }
        } catch { /* ignore */ }
    }

    // Fallback: check smooth providers
    const providersPath = join(homedir(), '.smooth', 'providers.json');
    if (existsSync(providersPath)) {
        try {
            const providers = JSON.parse(readFileSync(providersPath, 'utf8'));
            const zen = providers.providers?.['opencode-zen'];
            if (zen?.apiKey && zen.apiKey !== 'opencode-managed') {
                return { apiKey: zen.apiKey, model: zen.model ?? 'opencode/claude-sonnet-4-6', baseUrl: 'https://opencode.ai/api/v1' };
            }

            // Check for direct provider keys (anthropic, openai, etc.)
            for (const [id, p] of Object.entries(providers.providers ?? {})) {
                const provider = p as { apiKey?: string; model?: string; baseUrl?: string; enabled?: boolean };
                if (!provider.enabled || !provider.apiKey) continue;
                if (id === 'anthropic') return { apiKey: provider.apiKey, model: provider.model ?? 'claude-sonnet-4-20250514', baseUrl: 'https://api.anthropic.com/v1' };
                if (id === 'openai') return { apiKey: provider.apiKey, model: provider.model ?? 'gpt-4o', baseUrl: 'https://api.openai.com/v1' };
                if (id === 'openrouter') return { apiKey: provider.apiKey, model: provider.model ?? 'anthropic/claude-sonnet-4', baseUrl: provider.baseUrl ?? 'https://openrouter.ai/api/v1' };
            }
        } catch { /* ignore */ }
    }

    return null;
}

/** Send a chat message to the leader — streams response from LLM */
chatRoutes.post('/', async (c) => {
    const body = await c.req.json();
    const parsed = ChatMessageSchema.parse(body);

    const auth = getOpenCodeAuth();
    if (!auth) {
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
            const response = await fetch(`${auth.baseUrl}/chat/completions`, {
                method: 'POST',
                headers: {
                    Authorization: `Bearer ${auth.apiKey}`,
                    'Content-Type': 'application/json',
                },
                body: JSON.stringify({
                    model: auth.model,
                    messages: [
                        {
                            role: 'system',
                            content: `You are Smooth, an AI agent orchestration leader. You help users manage projects, assign work to Smooth Operators (AI agents in sandboxes), review work, and coordinate tasks via Beads.

Available commands the user can run:
- th run <bead-id> — trigger work on a bead
- th operators — list active operators
- th pause/steer/cancel <bead-id> — control operators
- th auth status — check auth
- th status — system health

You can help with: explaining the system, suggesting which beads to work on, reviewing operator output, answering questions about the codebase being worked on.`,
                        },
                        { role: 'user', content: parsed.content },
                    ],
                    stream: true,
                }),
            });

            if (!response.ok) {
                const errBody = await response.text();
                await stream.writeSSE({
                    event: 'text',
                    data: JSON.stringify({ type: 'text', content: `LLM error (${response.status}): ${errBody.slice(0, 200)}` }),
                });
                await stream.writeSSE({ event: 'done', data: JSON.stringify({ type: 'done', content: '' }) });
                return;
            }

            const reader = response.body?.getReader();
            if (!reader) {
                await stream.writeSSE({
                    event: 'text',
                    data: JSON.stringify({ type: 'text', content: 'No response stream from LLM.' }),
                });
                await stream.writeSSE({ event: 'done', data: JSON.stringify({ type: 'done', content: '' }) });
                return;
            }

            const decoder = new TextDecoder();
            let fullContent = '';

            while (true) {
                const { done, value } = await reader.read();
                if (done) break;

                const chunk = decoder.decode(value);
                for (const line of chunk.split('\n')) {
                    if (!line.startsWith('data: ')) continue;
                    const data = line.slice(6);
                    if (data === '[DONE]') break;

                    try {
                        const json = JSON.parse(data);
                        const delta = json.choices?.[0]?.delta?.content;
                        if (delta) {
                            fullContent += delta;
                            await stream.writeSSE({
                                event: 'text',
                                data: JSON.stringify({ type: 'text', content: delta }),
                            });
                        }
                    } catch { /* skip malformed chunks */ }
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

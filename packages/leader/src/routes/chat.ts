import { Hono } from 'hono';
import { streamSSE } from 'hono/streaming';
import { execSync, spawn } from 'node:child_process';

import { ChatMessageSchema } from '@smooai/smooth-shared/schemas';
import { createAuditLogger } from '@smooai/smooth-shared/audit-log';

export const chatRoutes = new Hono();

const audit = createAuditLogger('leader');

/** Find the opencode binary */
function findOpencode(): string {
    const paths = [`${process.env.HOME}/.opencode/bin/opencode`, '/opt/homebrew/bin/opencode', '/usr/local/bin/opencode', 'opencode'];
    for (const p of paths) {
        try {
            execSync(`${p} --version`, { stdio: 'pipe' });
            return p;
        } catch {
            /* continue */
        }
    }
    return 'opencode';
}

const OPENCODE_BIN = findOpencode();

/** Send a chat message — uses opencode run for LLM access (CLI > everything) */
chatRoutes.post('/', async (c) => {
    const body = await c.req.json();
    const parsed = ChatMessageSchema.parse(body);

    audit.promptSent('chat', parsed.content);
    const start = Date.now();

    return streamSSE(c, async (stream) => {
        try {
            // Use opencode run — handles auth, model selection, streaming
            const proc = spawn(OPENCODE_BIN, ['run', parsed.content, '-m', 'opencode/claude-sonnet-4-6'], {
                stdio: ['pipe', 'pipe', 'pipe'],
                env: { ...process.env, TERM: 'dumb', NO_COLOR: '1' },
            });

            let fullContent = '';

            proc.stdout.on('data', async (chunk: Buffer) => {
                // Strip ANSI codes
                const clean = chunk.toString().replace(/\x1b\[[0-9;]*[a-zA-Z]/g, '').replace(/\x1b\][^\x07]*\x07/g, '');

                if (clean.trim()) {
                    fullContent += clean;
                    await stream.writeSSE({
                        event: 'text',
                        data: JSON.stringify({ type: 'text', content: clean }),
                    });
                }
            });

            await new Promise<void>((resolve, reject) => {
                proc.on('close', (code) => {
                    if (code !== 0 && !fullContent) {
                        reject(new Error(`opencode exited with code ${code}`));
                    } else {
                        resolve();
                    }
                });
                proc.on('error', reject);
            });

            audit.promptReceived('chat', fullContent.slice(0, 200), Date.now() - start);
            await stream.writeSSE({ event: 'done', data: JSON.stringify({ type: 'done', content: '' }) });
        } catch (error) {
            const msg = error instanceof Error ? error.message : String(error);
            await stream.writeSSE({
                event: 'text',
                data: JSON.stringify({ type: 'text', content: `Error: ${msg}. Is opencode authenticated? Run: th auth login opencode-zen` }),
            });
            await stream.writeSSE({ event: 'done', data: JSON.stringify({ type: 'done', content: '' }) });
        }
    });
});

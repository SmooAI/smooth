/** Exec route — run allowlisted commands inside operator sandboxes */

import { Hono } from 'hono';

import { getBackend } from '../backend/registry.js';

export const execRoutes = new Hono();

/** Allowlisted command prefixes that operators can run */
const ALLOWED_COMMANDS = ['npx', 'pnpm', 'npm', 'node', 'grep', 'find', 'cat', 'head', 'tail', 'wc', 'sort', 'ls', 'git', 'python', 'pytest', 'ruff', 'vitest', 'jest', 'tsc', 'oxlint'];

execRoutes.post('/:id/exec', async (c) => {
    const sandboxId = c.req.param('id');
    const body = await c.req.json();
    const { command } = body as { command: string[] };

    if (!command || !Array.isArray(command) || command.length === 0) {
        return c.json({ error: 'command must be a non-empty array', ok: false }, 400);
    }

    // Validate command against allowlist
    const baseCommand = command[0];
    if (!ALLOWED_COMMANDS.includes(baseCommand)) {
        return c.json({ error: `Command '${baseCommand}' is not allowed. Allowed: ${ALLOWED_COMMANDS.join(', ')}`, ok: false }, 403);
    }

    const backend = getBackend();

    try {
        const result = await backend.exec(sandboxId, command);
        return c.json(result);
    } catch (error) {
        return c.json({ error: (error as Error).message, ok: false }, 500);
    }
});

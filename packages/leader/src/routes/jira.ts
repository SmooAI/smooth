import { Hono } from 'hono';

import { jiraSync } from '../beads/client.js';

export const jiraRoutes = new Hono();

/** Trigger Jira sync */
jiraRoutes.post('/sync', async (c) => {
    const body = await c.req.json().catch(() => ({}));
    const direction = (body as Record<string, string>).direction as 'pull' | 'push' | undefined;

    const result = await jiraSync(direction);
    return c.json({ data: { output: result }, ok: true });
});

/** Get Jira sync status */
jiraRoutes.get('/status', async (c) => {
    // TODO: Track last sync time in config table
    return c.json({
        data: {
            lastSync: null,
            pendingChanges: 0,
            conflicts: 0,
            connected: false,
        },
        ok: true,
    });
});

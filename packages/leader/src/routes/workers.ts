import { Hono } from 'hono';

import { db, eq } from '@smooai/smooth-db/index';
import { workerRuns } from '@smooai/smooth-db/schema/workers';

import { getBackend } from '../backend/registry.js';

export const workersRoutes = new Hono();

/** List active Smooth Operators */
workersRoutes.get('/', async (c) => {
    const workers = await db.select().from(workerRuns).where(eq(workerRuns.status, 'running'));
    return c.json({ data: workers, ok: true });
});

/** Get a specific Smooth Operator */
workersRoutes.get('/:id', async (c) => {
    const id = c.req.param('id');
    const [worker] = await db.select().from(workerRuns).where(eq(workerRuns.workerId, id));

    if (!worker) {
        return c.json({ error: 'Smooth Operator not found', ok: false, statusCode: 404 }, 404);
    }

    return c.json({ data: worker, ok: true });
});

/** Kill a Smooth Operator */
workersRoutes.delete('/:id', async (c) => {
    const id = c.req.param('id');

    // Destroy the sandbox via backend
    const backend = getBackend();
    await backend.destroySandbox(id);

    // Update database record
    await db.update(workerRuns).set({ status: 'failed', completedAt: new Date() }).where(eq(workerRuns.workerId, id));

    console.log(`[workers] Killed Smooth Operator ${id}`);

    return c.json({ ok: true });
});

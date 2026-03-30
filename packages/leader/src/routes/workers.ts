import { Hono } from 'hono';
import { eq } from 'drizzle-orm';

import { db } from '@smooth/db/client';
import { workerRuns } from '@smooth/db/schema/workers';

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

    await db.update(workerRuns).set({ status: 'failed', completedAt: new Date() }).where(eq(workerRuns.workerId, id));

    // TODO: Actually stop the Docker container (Phase 2)
    console.log(`[workers] Killed Smooth Operator ${id}`);

    return c.json({ ok: true });
});

import { Hono } from 'hono';

import { BeadFiltersSchema } from '@smooth/shared/schemas';

import { getBead, getGraph, getReady, listBeads, search } from '../beads/client.js';

export const beadsRoutes = new Hono();

/** List beads with optional filters */
beadsRoutes.get('/', async (c) => {
    const query = c.req.query();
    const filters = BeadFiltersSchema.parse(query);
    const beads = await listBeads(filters);
    return c.json({ data: beads, ok: true });
});

/** Get ready beads (open, no blockers) */
beadsRoutes.get('/ready', async (c) => {
    const beads = await getReady();
    return c.json({ data: beads, ok: true });
});

/** Search beads */
beadsRoutes.get('/search', async (c) => {
    const q = c.req.query('q') ?? '';
    const beads = await search(q);
    return c.json({ data: beads, ok: true });
});

/** Get bead graph */
beadsRoutes.get('/graph', async (c) => {
    const graph = await getGraph();
    return c.json({ data: graph, ok: true });
});

/** Get a specific bead with full detail */
beadsRoutes.get('/:id', async (c) => {
    const bead = await getBead(c.req.param('id'));
    return c.json({ data: bead, ok: true });
});

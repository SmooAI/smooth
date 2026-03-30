import { Hono } from 'hono';

import { CreateProjectSchema } from '@smooth/shared/schemas';

import { createBead, listBeads } from '../beads/client.js';

export const projectsRoutes = new Hono();

/** List all projects */
projectsRoutes.get('/', async (c) => {
    const projects = await listBeads({ type: 'project' });
    return c.json({ data: projects, ok: true });
});

/** Get a specific project */
projectsRoutes.get('/:id', async (c) => {
    const { getBead } = await import('../beads/client.js');
    const bead = await getBead(c.req.param('id'));
    return c.json({ data: bead, ok: true });
});

/** Create a project */
projectsRoutes.post('/', async (c) => {
    const body = await c.req.json();
    const parsed = CreateProjectSchema.parse(body);

    const id = await createBead({
        title: parsed.name,
        description: parsed.description,
        type: 'project',
        priority: 2,
    });

    return c.json({ data: { id, name: parsed.name }, ok: true }, 201);
});

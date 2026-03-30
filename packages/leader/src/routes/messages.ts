import { Hono } from 'hono';

import { SendMessageSchema } from '@smooth/shared/schemas';

import { readMessages, sendMessage, getInbox } from '../beads/messaging.js';

export const messagesRoutes = new Hono();

/** Get inbox (messages requiring human attention) */
messagesRoutes.get('/inbox', async (c) => {
    const inbox = await getInbox();
    return c.json({ data: inbox, ok: true });
});

/** Get messages for a bead */
messagesRoutes.get('/:beadId', async (c) => {
    const beadId = c.req.param('beadId');
    const direction = c.req.query('direction') as never;
    const messages = await readMessages(beadId, direction || undefined);
    return c.json({ data: messages, ok: true });
});

/** Send a message on a bead */
messagesRoutes.post('/', async (c) => {
    const body = await c.req.json();
    const parsed = SendMessageSchema.parse(body);

    await sendMessage(parsed.beadId, parsed.direction, parsed.content, 'human');

    return c.json({ ok: true }, 201);
});

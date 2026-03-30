import { Hono } from 'hono';

import { listBeads, updateBead } from '../beads/client.js';
import { sendMessage } from '../beads/messaging.js';

export const reviewsRoutes = new Hono();

/** List pending reviews */
reviewsRoutes.get('/', async (c) => {
    const reviews = await listBeads({ label: 'review:pending' });
    return c.json({ data: reviews, ok: true });
});

/** Approve a review */
reviewsRoutes.post('/:beadId/approve', async (c) => {
    const beadId = c.req.param('beadId');

    await updateBead(beadId, { removeLabel: 'review:pending', addLabel: 'review:approved' });
    await sendMessage(beadId, 'human→leader', 'Review approved.', 'human');

    return c.json({ ok: true });
});

/** Reject a review */
reviewsRoutes.post('/:beadId/reject', async (c) => {
    const beadId = c.req.param('beadId');
    const body = await c.req.json().catch(() => ({}));
    const reason = (body as Record<string, string>).reason ?? 'Rejected';

    await updateBead(beadId, { removeLabel: 'review:pending', addLabel: 'review:rejected' });
    await sendMessage(beadId, 'human→leader', `Review rejected: ${reason}`, 'human');

    return c.json({ ok: true });
});

/** Request rework */
reviewsRoutes.post('/:beadId/rework', async (c) => {
    const beadId = c.req.param('beadId');
    const body = await c.req.json().catch(() => ({}));
    const feedback = (body as Record<string, string>).feedback ?? 'Needs rework';

    await updateBead(beadId, { removeLabel: 'review:pending', addLabel: 'review:rework' });
    await sendMessage(beadId, 'human→leader', `Rework requested: ${feedback}`, 'human');

    return c.json({ ok: true });
});

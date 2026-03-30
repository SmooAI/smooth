/** Steering routes — pause, redirect, resume, cancel operators mid-task */

import { Hono } from 'hono';

import { getBackend } from '../backend/registry.js';
import { updateBead } from '../beads/client.js';
import { sendMessage } from '../beads/messaging.js';

export const steeringRoutes = new Hono();

/** Pause an operator */
steeringRoutes.post('/:beadId/pause', async (c) => {
    const beadId = c.req.param('beadId');
    await sendMessage(beadId, 'leader→worker', '[STEERING:PAUSE] Operator paused by human. Waiting for resume.', 'leader');
    await updateBead(beadId, { addLabel: 'steering:paused' });
    return c.json({ ok: true, action: 'paused' });
});

/** Inject steering guidance */
steeringRoutes.post('/:beadId/steer', async (c) => {
    const beadId = c.req.param('beadId');
    const body = await c.req.json();
    const { message } = body as { message: string };

    if (!message) return c.json({ error: 'message required', ok: false }, 400);

    await sendMessage(beadId, 'leader→worker', `[STEERING:GUIDANCE] ${message}`, 'leader');
    return c.json({ ok: true, action: 'steered' });
});

/** Resume a paused operator */
steeringRoutes.post('/:beadId/resume', async (c) => {
    const beadId = c.req.param('beadId');
    await sendMessage(beadId, 'leader→worker', '[STEERING:RESUME] Operator resumed by human.', 'leader');
    await updateBead(beadId, { removeLabel: 'steering:paused' });
    return c.json({ ok: true, action: 'resumed' });
});

/** Cancel an operator */
steeringRoutes.post('/:beadId/cancel', async (c) => {
    const beadId = c.req.param('beadId');
    await sendMessage(beadId, 'leader→worker', '[STEERING:CANCEL] Operator cancelled by human.', 'leader');
    await updateBead(beadId, { addLabel: 'steering:cancelled' });

    // Kill the sandbox if we can find the worker
    const backend = getBackend();
    const sandboxes = await backend.listSandboxes();
    const sandbox = sandboxes.find((s) => s.beadId === beadId);
    if (sandbox) {
        await backend.destroySandbox(sandbox.sandboxId);
    }

    return c.json({ ok: true, action: 'cancelled' });
});

/** beads_message — Send/read messages via beads */

import { z } from 'zod';

import type { SmoothTool, ToolContext } from '../types.js';

export const beadsMessageTool: SmoothTool = {
    name: 'beads_message',
    description: 'Send a message on a bead or read messages from a bead',
    inputSchema: z.object({
        action: z.enum(['send', 'read']),
        beadId: z.string().optional().describe('Bead ID. Defaults to current bead.'),
        content: z.string().optional().describe('Message content (for send action)'),
        direction: z.enum(['worker→leader', 'worker→worker']).optional().default('worker→leader'),
    }),
    outputSchema: z.object({
        ok: z.boolean(),
        messages: z.array(z.unknown()).optional(),
    }),
    permissions: ['beads:message'],
    logToBeads: true,
    handler: async (input, ctx) => {
        const beadId = input.beadId ?? ctx.beadId;

        if (input.action === 'send') {
            if (!input.content) throw new Error('Content required for send action');

            await fetch(`${ctx.leaderUrl}/api/messages`, {
                method: 'POST',
                headers: { 'Content-Type': 'application/json' },
                body: JSON.stringify({
                    beadId,
                    content: input.content,
                    direction: input.direction,
                }),
            });
            return { ok: true };
        }

        // Read
        const response = await fetch(`${ctx.leaderUrl}/api/messages/${beadId}`);
        const data = await response.json() as { data: unknown[] };
        return { ok: true, messages: data.data };
    },
};

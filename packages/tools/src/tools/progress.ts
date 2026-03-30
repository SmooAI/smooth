/** progress_append — Append progress update to current bead */

import { z } from 'zod';

import type { SmoothTool, ToolContext } from '../types.js';

export const progressAppendTool: SmoothTool = {
    name: 'progress_append',
    description: 'Append a progress update to the current bead. Use this regularly to keep work observable.',
    inputSchema: z.object({
        content: z.string().describe('Progress update message'),
        beadId: z.string().optional().describe('Bead ID. Defaults to current bead.'),
    }),
    outputSchema: z.object({ ok: z.boolean() }),
    permissions: ['beads:write'],
    logToBeads: false, // This IS the log
    handler: async (input, ctx) => {
        const beadId = input.beadId ?? ctx.beadId;

        await fetch(`${ctx.leaderUrl}/api/messages`, {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({
                beadId,
                content: input.content,
                direction: 'progress',
            }),
        });
        return { ok: true };
    },
};

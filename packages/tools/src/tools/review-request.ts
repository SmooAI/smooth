/** review_request — Request adversarial review */

import { z } from 'zod';

import type { SmoothTool } from '../types.js';

export const reviewRequestTool: SmoothTool = {
    name: 'review_request',
    description: 'Request an adversarial review of the completed work. A review Smooth Operator will inspect diffs, tests, and artifacts.',
    inputSchema: z.object({
        summary: z.string().describe('Summary of work completed for the reviewer'),
        beadId: z.string().optional(),
    }),
    outputSchema: z.object({ ok: z.boolean() }),
    permissions: ['beads:write'],
    logToBeads: true,
    handler: async (input, ctx) => {
        const beadId = input.beadId ?? ctx.beadId;

        // Send review request to leader
        await fetch(`${ctx.leaderUrl}/api/messages`, {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({
                beadId,
                content: `Review requested: ${input.summary}`,
                direction: 'worker→leader',
            }),
        });

        return { ok: true };
    },
};

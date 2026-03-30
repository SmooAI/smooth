/** workflow_transition — Update bead phase label */

import { z } from 'zod';

import type { SmoothTool } from '../types.js';

export const workflowTransitionTool: SmoothTool = {
    name: 'workflow_transition',
    description: 'Transition the current bead to a new workflow phase',
    inputSchema: z.object({
        phase: z.enum(['assess', 'plan', 'orchestrate', 'execute', 'finalize']),
        beadId: z.string().optional(),
    }),
    outputSchema: z.object({ ok: z.boolean() }),
    permissions: ['beads:write'],
    logToBeads: true,
    handler: async (input, ctx) => {
        const beadId = input.beadId ?? ctx.beadId;

        // Report transition to leader
        await fetch(`${ctx.leaderUrl}/api/messages`, {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({
                beadId,
                content: `Transitioning to phase: ${input.phase}`,
                direction: 'worker→leader',
            }),
        });

        return { ok: true };
    },
};

/** spawn_subtask — Create child bead for sub-work */

import { z } from 'zod';

import type { SmoothTool } from '../types.js';

export const spawnSubtaskTool: SmoothTool = {
    name: 'spawn_subtask',
    description: 'Create a child bead (subtask) linked to the current bead. The leader will assign a Smooth Operator to it.',
    inputSchema: z.object({
        title: z.string().describe('Subtask title'),
        description: z.string().describe('What needs to be done'),
        priority: z.number().min(0).max(4).optional().default(2),
        parentBeadId: z.string().optional().describe('Parent bead ID. Defaults to current bead.'),
    }),
    outputSchema: z.object({
        ok: z.boolean(),
        subtaskId: z.string().optional(),
    }),
    permissions: ['beads:write'],
    logToBeads: true,
    handler: async (input, ctx) => {
        const parentBeadId = input.parentBeadId ?? ctx.beadId;

        // Create subtask via leader API
        const response = await fetch(`${ctx.leaderUrl}/api/projects`, {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({
                name: input.title,
                description: `${input.description}\n\nParent: ${parentBeadId}`,
            }),
        });

        const data = (await response.json()) as { data: { id: string } };

        // Report to leader
        await fetch(`${ctx.leaderUrl}/api/messages`, {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({
                beadId: parentBeadId,
                content: `Spawned subtask ${data.data.id}: ${input.title}`,
                direction: 'worker→leader',
            }),
        });

        return { ok: true, subtaskId: data.data.id };
    },
};

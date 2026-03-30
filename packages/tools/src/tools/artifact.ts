/** artifact_write — Write artifact and link to bead */

import { z } from 'zod';

import type { SmoothTool, ToolContext } from '../types.js';

export const artifactWriteTool: SmoothTool = {
    name: 'artifact_write',
    description: 'Record an artifact (diff, test results, summary, code, document, data) and link it to the current bead',
    inputSchema: z.object({
        type: z.enum(['diff', 'test-results', 'summary', 'code', 'document', 'data']),
        path: z.string().describe('Path to the artifact file'),
        description: z.string().optional().describe('Description of the artifact'),
        beadId: z.string().optional(),
    }),
    outputSchema: z.object({ ok: z.boolean() }),
    permissions: ['beads:write', 'fs:write'],
    logToBeads: true,
    handler: async (input, ctx) => {
        const beadId = input.beadId ?? ctx.beadId;
        const desc = input.description ? ` — ${input.description}` : '';

        await fetch(`${ctx.leaderUrl}/api/messages`, {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({
                beadId,
                content: `[${input.type}] ${input.path}${desc}`,
                direction: 'artifact',
            }),
        });
        return { ok: true };
    },
};

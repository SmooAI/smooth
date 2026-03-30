/** beads_context — Read current bead + neighbors + thread */

import { z } from 'zod';

import type { SmoothTool, ToolContext } from '../types.js';

export const beadsContextTool: SmoothTool = {
    name: 'beads_context',
    description: 'Read the current bead details including description, status, dependencies, and message thread',
    inputSchema: z.object({
        beadId: z.string().optional().describe('Bead ID to inspect. Defaults to current bead.'),
        includeNeighbors: z.boolean().optional().default(true).describe('Include dependency graph neighbors'),
    }),
    outputSchema: z.object({
        bead: z.unknown(),
        neighbors: z.array(z.unknown()).optional(),
        thread: z.array(z.unknown()).optional(),
    }),
    permissions: ['beads:read'],
    logToBeads: false,
    handler: async (input, ctx) => {
        const beadId = input.beadId ?? ctx.beadId;
        const response = await fetch(`${ctx.leaderUrl}/api/beads/${beadId}`);
        const data = await response.json() as { data: unknown };

        let neighbors: unknown[] = [];
        if (input.includeNeighbors) {
            // Get related beads via graph
            try {
                const graphResponse = await fetch(`${ctx.leaderUrl}/api/beads/graph`);
                const graphData = await graphResponse.json() as { data: unknown };
                neighbors = Array.isArray(graphData.data) ? graphData.data : [];
            } catch { /* best effort */ }
        }

        // Get message thread
        const threadResponse = await fetch(`${ctx.leaderUrl}/api/messages/${beadId}`);
        const threadData = await threadResponse.json() as { data: unknown[] };

        return {
            bead: data.data,
            neighbors,
            thread: threadData.data,
        };
    },
};

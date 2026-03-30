/** code_search tool — grep inside the workspace */

import { z } from 'zod';

import type { SmoothTool } from '../types.js';

export const codeSearchTool: SmoothTool = {
    name: 'code_search',
    description: 'Search for patterns in the workspace code using grep. Returns matching lines with file paths and line numbers.',
    inputSchema: z.object({
        pattern: z.string().describe('Regex pattern to search for'),
        glob: z.string().optional().describe('File glob filter (e.g., "*.ts")'),
        path: z.string().optional().default('.').describe('Directory to search'),
        maxResults: z.number().optional().default(50).describe('Maximum results'),
    }),
    outputSchema: z.object({
        matches: z.array(z.object({ file: z.string(), line: z.number(), content: z.string() })),
        totalMatches: z.number(),
        truncated: z.boolean(),
    }),
    permissions: ['fs:read'],
    logToBeads: false,
    handler: async (input, ctx) => {
        const inp = input as { pattern: string; glob?: string; path: string; maxResults: number };
        const args = ['grep', '-rn', '--color=never'];
        if (inp.glob) args.push(`--include=${inp.glob}`);
        args.push('-E', inp.pattern, inp.path);

        const response = await fetch(`${ctx.leaderUrl}/api/workers/${ctx.workerId}/exec`, {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({ command: args }),
        });

        const result = (await response.json()) as { stdout: string; exitCode: number };
        const lines = result.stdout.trim().split('\n').filter(Boolean);
        const truncated = lines.length > inp.maxResults;
        const matches = lines.slice(0, inp.maxResults).map((line) => {
            const match = line.match(/^([^:]+):(\d+):(.*)$/);
            if (match) return { file: match[1], line: parseInt(match[2]), content: match[3].trim() };
            return { file: '', line: 0, content: line };
        });

        return { matches, totalMatches: lines.length, truncated };
    },
};

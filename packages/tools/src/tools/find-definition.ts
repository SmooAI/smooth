/** find_definition tool — find where a symbol is defined in the codebase */

import { z } from 'zod';

import type { SmoothTool } from '../types.js';

export const findDefinitionTool: SmoothTool = {
    name: 'find_definition',
    description: 'Find where a function, class, interface, type, or variable is defined in the codebase.',
    inputSchema: z.object({
        symbol: z.string().describe('Symbol name to find'),
        path: z.string().optional().default('.').describe('Directory to search'),
    }),
    outputSchema: z.object({
        definitions: z.array(z.object({ file: z.string(), line: z.number(), content: z.string(), kind: z.string() })),
    }),
    permissions: ['fs:read'],
    logToBeads: false,
    handler: async (input, ctx) => {
        const inp = input as { symbol: string; path: string };

        // Search for export/declaration patterns
        const pattern = `(export\\s+)?(function|class|interface|type|const|enum|let|var)\\s+${inp.symbol}\\b`;
        const response = await fetch(`${ctx.leaderUrl}/api/workers/${ctx.workerId}/exec`, {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({
                command: ['grep', '-rn', '--include=*.ts', '--include=*.tsx', '--include=*.js', '--include=*.py', '-E', pattern, inp.path],
            }),
        });

        const result = (await response.json()) as { stdout: string };
        const definitions = result.stdout
            .trim()
            .split('\n')
            .filter(Boolean)
            .slice(0, 20)
            .map((line) => {
                const match = line.match(/^([^:]+):(\d+):(.*)$/);
                if (!match) return null;
                const content = match[3].trim();
                const kindMatch = content.match(/(?:export\s+)?(?:default\s+)?(function|class|interface|type|const|enum|let|var)/);
                return { file: match[1], line: parseInt(match[2]), content, kind: kindMatch?.[1] ?? 'unknown' };
            })
            .filter(Boolean) as Array<{ file: string; line: number; content: string; kind: string }>;

        return { definitions };
    },
};

/** repo_map tool — generates a file tree with key exports for codebase understanding */

import { z } from 'zod';

import type { SmoothTool } from '../types.js';

export const repoMapTool: SmoothTool = {
    name: 'repo_map',
    description: 'Generate a map of the workspace: file tree with key exports (functions, classes, interfaces). Useful for understanding unfamiliar codebases.',
    inputSchema: z.object({
        path: z.string().optional().default('.').describe('Directory to map'),
        depth: z.number().optional().default(3).describe('Max directory depth'),
        includeExports: z.boolean().optional().default(true).describe('Include exported symbols'),
    }),
    outputSchema: z.object({
        tree: z.string(),
        exports: z.array(z.object({ file: z.string(), symbols: z.array(z.string()) })),
        fileCount: z.number(),
    }),
    permissions: ['fs:read'],
    logToBeads: false,
    handler: async (input, ctx) => {
        const inp = input as { path: string; depth: number; includeExports: boolean };

        // Get file tree via exec
        const treeResp = await fetch(`${ctx.leaderUrl}/api/workers/${ctx.workerId}/exec`, {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({
                command: [
                    'find',
                    inp.path,
                    '-maxdepth',
                    String(inp.depth),
                    '-type',
                    'f',
                    '-name',
                    '*.ts',
                    '-o',
                    '-name',
                    '*.tsx',
                    '-o',
                    '-name',
                    '*.js',
                    '-o',
                    '-name',
                    '*.py',
                ],
            }),
        });

        const treeResult = (await treeResp.json()) as { stdout: string };
        const files = treeResult.stdout.trim().split('\n').filter(Boolean);

        let exports: Array<{ file: string; symbols: string[] }> = [];

        if (inp.includeExports && files.length <= 100) {
            // Grep for exports in TypeScript/JavaScript files
            const exportResp = await fetch(`${ctx.leaderUrl}/api/workers/${ctx.workerId}/exec`, {
                method: 'POST',
                headers: { 'Content-Type': 'application/json' },
                body: JSON.stringify({
                    command: [
                        'grep',
                        '-rn',
                        '--include=*.ts',
                        '--include=*.tsx',
                        '-E',
                        'export\\s+(function|class|interface|type|const|enum|default)',
                        inp.path,
                    ],
                }),
            });

            const exportResult = (await exportResp.json()) as { stdout: string };
            const exportMap = new Map<string, string[]>();

            for (const line of exportResult.stdout.split('\n').filter(Boolean).slice(0, 200)) {
                const match = line.match(/^([^:]+):\d+:\s*export\s+(?:default\s+)?(?:function|class|interface|type|const|enum)\s+(\w+)/);
                if (match) {
                    const [, file, symbol] = match;
                    if (!exportMap.has(file)) exportMap.set(file, []);
                    exportMap.get(file)!.push(symbol);
                }
            }

            exports = Array.from(exportMap.entries()).map(([file, symbols]) => ({ file, symbols }));
        }

        // Build tree string
        const tree = files.slice(0, 200).join('\n');

        return { tree, exports, fileCount: files.length };
    },
};

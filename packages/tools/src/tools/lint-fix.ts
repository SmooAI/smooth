/** lint_fix tool — runs the project's linter inside the sandbox */

import { z } from 'zod';

import type { SmoothTool } from '../types.js';

export const lintFixTool: SmoothTool = {
    name: 'lint_fix',
    description: 'Run the project linter with auto-fix. Returns structured results with errors, warnings, and fix count.',
    inputSchema: z.object({
        path: z.string().optional().describe('Path to lint (default: entire workspace)'),
        fix: z.boolean().optional().default(true).describe('Auto-fix issues'),
    }),
    outputSchema: z.object({
        errors: z.number(),
        warnings: z.number(),
        fixed: z.number(),
        details: z.array(z.object({ file: z.string(), line: z.number().optional(), message: z.string(), severity: z.string() })),
        passed: z.boolean(),
    }),
    permissions: ['exec:test'],
    logToBeads: true,
    handler: async (input, ctx) => {
        const path = (input as { path?: string }).path ?? '.';

        // Call the leader's exec endpoint to run lint in the sandbox
        const response = await fetch(`${ctx.leaderUrl}/api/workers/${ctx.workerId}/exec`, {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({ command: ['npx', 'oxlint', '--fix', path] }),
        });

        if (!response.ok) {
            return { errors: 1, warnings: 0, fixed: 0, details: [{ file: '', message: 'Failed to run linter', severity: 'error' }], passed: false };
        }

        const result = (await response.json()) as { stdout: string; stderr: string; exitCode: number };

        // Parse lint output (basic pattern matching)
        const errorCount = (result.stdout.match(/\d+ error/)?.[0] ?? '0').match(/\d+/)?.[0] ?? '0';
        const warnCount = (result.stdout.match(/\d+ warning/)?.[0] ?? '0').match(/\d+/)?.[0] ?? '0';

        return {
            errors: parseInt(errorCount),
            warnings: parseInt(warnCount),
            fixed: 0,
            details: [],
            passed: result.exitCode === 0,
        };
    },
};

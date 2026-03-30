/** test_run tool — runs the project's test suite inside the sandbox */

import { z } from 'zod';

import type { SmoothTool } from '../types.js';

export const testRunTool: SmoothTool = {
    name: 'test_run',
    description: 'Run the project test suite. Returns structured results with pass/fail counts and error details.',
    inputSchema: z.object({
        pattern: z.string().optional().describe('Test file pattern or test name filter'),
        file: z.string().optional().describe('Specific test file to run'),
    }),
    outputSchema: z.object({
        passed: z.number(),
        failed: z.number(),
        skipped: z.number(),
        errors: z.array(z.object({ test: z.string(), message: z.string(), file: z.string().optional() })),
        allPassed: z.boolean(),
        output: z.string(),
    }),
    permissions: ['exec:test'],
    logToBeads: true,
    handler: async (input, ctx) => {
        const args = ['npx', 'vitest', 'run', '--reporter=json'];
        const inp = input as { pattern?: string; file?: string };
        if (inp.file) args.push(inp.file);
        if (inp.pattern) args.push('-t', inp.pattern);

        const response = await fetch(`${ctx.leaderUrl}/api/workers/${ctx.workerId}/exec`, {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({ command: args }),
        });

        if (!response.ok) {
            return { passed: 0, failed: 1, skipped: 0, errors: [{ test: '', message: 'Failed to run tests' }], allPassed: false, output: '' };
        }

        const result = (await response.json()) as { stdout: string; stderr: string; exitCode: number };

        // Try to parse vitest JSON output
        try {
            const json = JSON.parse(result.stdout);
            return {
                passed: json.numPassedTests ?? 0,
                failed: json.numFailedTests ?? 0,
                skipped: json.numPendingTests ?? 0,
                errors: (json.testResults ?? [])
                    .flatMap((tr: any) =>
                        (tr.assertionResults ?? [])
                            .filter((ar: any) => ar.status === 'failed')
                            .map((ar: any) => ({ test: ar.fullName, message: ar.failureMessages?.[0] ?? '', file: tr.name })),
                    )
                    .slice(0, 10),
                allPassed: json.numFailedTests === 0,
                output: result.stdout.slice(0, 2000),
            };
        } catch {
            // Fallback: parse text output
            const passMatch = result.stdout.match(/(\d+)\s+passed/);
            const failMatch = result.stdout.match(/(\d+)\s+failed/);
            return {
                passed: parseInt(passMatch?.[1] ?? '0'),
                failed: parseInt(failMatch?.[1] ?? '0'),
                skipped: 0,
                errors: result.exitCode !== 0 ? [{ test: '', message: result.stderr.slice(0, 500) }] : [],
                allPassed: result.exitCode === 0,
                output: result.stdout.slice(0, 2000),
            };
        }
    },
};

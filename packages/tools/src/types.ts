import type { z } from 'zod';

import type { ToolPermission } from '@smooai/smooth-shared/worker-types';

export interface ToolContext {
    beadId: string;
    workerId: string;
    runId: string;
    leaderUrl: string;
    permissions: ToolPermission[];
}

// Using `any` for handler I/O so tool definitions don't need explicit generic params.
// Input/output are validated at runtime by Zod schemas in the registry.
// eslint-disable-next-line @typescript-eslint/no-explicit-any
export interface SmoothTool {
    name: string;
    description: string;
    inputSchema: z.ZodType;
    outputSchema: z.ZodType;
    permissions: ToolPermission[];
    logToBeads: boolean;
    handler: (input: any, ctx: ToolContext) => Promise<any>;
}

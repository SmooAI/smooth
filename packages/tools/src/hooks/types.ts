/** Hook types for operator guardrails */

import type { ToolContext } from '../types.js';

export type HookEvent = 'pre-tool' | 'post-tool';

export interface HookContext {
    toolName: string;
    input: unknown;
    output?: unknown;
    toolContext: ToolContext;
}

export interface HookResult {
    allow: boolean;
    reason?: string;
}

export interface Hook {
    name: string;
    event: HookEvent;
    /** Which tools this hook applies to. Empty array = all tools. */
    tools: string[];
    handler: (ctx: HookContext) => Promise<HookResult>;
}

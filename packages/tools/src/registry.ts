/** Tool registry — manages available tools, hooks, and executes them with context */

import type { ToolPermission } from '@smooai/smooth-shared/worker-types';

import { createAuditLogger } from '@smooai/smooth-shared/audit-log';

import type { Hook } from './hooks/types.js';
import type { SmoothTool, ToolContext } from './types.js';

import { HookPipeline } from './hooks/pipeline.js';

export class ToolRegistry {
    private tools = new Map<string, SmoothTool>();
    private pipeline = new HookPipeline();

    register(tool: SmoothTool): void {
        this.tools.set(tool.name, tool);
    }

    registerHook(hook: Hook): void {
        this.pipeline.register(hook);
    }

    get(name: string): SmoothTool | undefined {
        return this.tools.get(name);
    }

    /** Get tools available for the given permissions */
    getAvailable(permissions: ToolPermission[]): SmoothTool[] {
        return Array.from(this.tools.values()).filter((tool) => tool.permissions.every((p) => permissions.includes(p)));
    }

    /** List all registered tools */
    listAll(): SmoothTool[] {
        return Array.from(this.tools.values());
    }

    /** Execute a tool with hooks, permission checking, and audit logging */
    async execute(name: string, input: unknown, ctx: ToolContext): Promise<unknown> {
        const tool = this.tools.get(name);
        if (!tool) {
            throw new Error(`Tool not found: ${name}`);
        }

        // Permission check
        const missingPerms = tool.permissions.filter((p) => !ctx.permissions.includes(p));
        if (missingPerms.length > 0) {
            throw new Error(`Insufficient permissions for tool ${name}: missing ${missingPerms.join(', ')}`);
        }

        // Validate input
        const parsed = tool.inputSchema.parse(input);

        // Run pre-hooks (guardrails)
        const preResult = await this.pipeline.runPreHooks(name, parsed, ctx);
        if (!preResult.allow) {
            throw new Error(`Hook blocked tool ${name}: ${preResult.reason}`);
        }

        // Execute and measure
        const start = Date.now();
        const audit = createAuditLogger(ctx.workerId, ctx.beadId);

        let result: unknown;
        try {
            result = await tool.handler(parsed, ctx);
        } catch (error) {
            const durationMs = Date.now() - start;
            audit.toolCall(name, parsed, undefined, durationMs);
            audit.error(`Tool ${name} failed: ${error instanceof Error ? error.message : String(error)}`);
            throw error;
        }

        const durationMs = Date.now() - start;

        // Validate output
        const validated = tool.outputSchema.parse(result);

        // Run post-hooks (tracking, validation)
        const postResult = await this.pipeline.runPostHooks(name, parsed, validated, ctx);
        if (!postResult.allow) {
            throw new Error(`Post-hook rejected tool ${name}: ${postResult.reason}`);
        }

        // Audit log — always
        audit.toolCall(name, parsed, validated, durationMs);

        // Also log to beads if configured
        if (tool.logToBeads) {
            await logToolCall(ctx, name, parsed);
        }

        return validated;
    }
}

async function logToolCall(ctx: ToolContext, toolName: string, input: unknown): Promise<void> {
    try {
        await fetch(`${ctx.leaderUrl}/api/messages`, {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({
                beadId: ctx.beadId,
                content: `[tool:${toolName}] input=${JSON.stringify(input).slice(0, 200)}`,
                direction: 'progress',
            }),
        });
    } catch {
        // Best-effort logging
    }
}

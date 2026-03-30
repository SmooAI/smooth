/** Tool registry — manages available tools and executes them with context */

import type { ToolPermission } from '@smooai/smooth-shared/worker-types';

import type { SmoothTool, ToolContext } from './types.js';

export class ToolRegistry {
    private tools = new Map<string, SmoothTool>();

    register(tool: SmoothTool): void {
        this.tools.set(tool.name, tool);
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

    /** Execute a tool with permission checking and logging */
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

        // Execute
        const result = await tool.handler(parsed, ctx);

        // Validate output
        const validated = tool.outputSchema.parse(result);

        // Log to beads if configured
        if (tool.logToBeads) {
            await logToolCall(ctx, name, parsed, validated);
        }

        return validated;
    }
}

async function logToolCall(ctx: ToolContext, toolName: string, input: unknown, _output: unknown): Promise<void> {
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

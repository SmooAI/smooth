import { describe, expect, it } from 'vitest';
import { z } from 'zod';

import { ToolRegistry } from './registry.js';
import type { SmoothTool, ToolContext } from './types.js';

const mockCtx: ToolContext = {
    beadId: 'test-bead',
    workerId: 'test-worker',
    runId: 'test-run',
    leaderUrl: 'http://localhost:4400',
    permissions: ['beads:read', 'beads:write'],
};

const echoTool: SmoothTool = {
    name: 'echo',
    description: 'Echoes input',
    inputSchema: z.object({ message: z.string() }),
    outputSchema: z.object({ echoed: z.string() }),
    permissions: ['beads:read'],
    logToBeads: false,
    handler: async (input) => ({ echoed: input.message }),
};

const writeTool: SmoothTool = {
    name: 'write',
    description: 'Writes something',
    inputSchema: z.object({}),
    outputSchema: z.object({ ok: z.boolean() }),
    permissions: ['beads:write', 'fs:write'],
    logToBeads: false,
    handler: async () => ({ ok: true }),
};

describe('ToolRegistry', () => {
    it('registers and retrieves tools', () => {
        const registry = new ToolRegistry();
        registry.register(echoTool);

        expect(registry.get('echo')).toBe(echoTool);
        expect(registry.get('nonexistent')).toBeUndefined();
    });

    it('lists all tools', () => {
        const registry = new ToolRegistry();
        registry.register(echoTool);
        registry.register(writeTool);

        expect(registry.listAll()).toHaveLength(2);
    });

    it('filters available tools by permissions', () => {
        const registry = new ToolRegistry();
        registry.register(echoTool);
        registry.register(writeTool);

        // Only beads:read permission
        const readOnly = registry.getAvailable(['beads:read']);
        expect(readOnly).toHaveLength(1);
        expect(readOnly[0].name).toBe('echo');

        // beads:read + beads:write + fs:write
        const full = registry.getAvailable(['beads:read', 'beads:write', 'fs:write']);
        expect(full).toHaveLength(2);
    });

    it('executes tools with valid input', async () => {
        const registry = new ToolRegistry();
        registry.register(echoTool);

        const result = await registry.execute('echo', { message: 'hello' }, mockCtx);
        expect(result).toEqual({ echoed: 'hello' });
    });

    it('throws on missing tool', async () => {
        const registry = new ToolRegistry();
        await expect(registry.execute('nonexistent', {}, mockCtx)).rejects.toThrow('Tool not found');
    });

    it('throws on insufficient permissions', async () => {
        const registry = new ToolRegistry();
        registry.register(writeTool);

        const limitedCtx = { ...mockCtx, permissions: ['beads:read' as const] };
        await expect(registry.execute('write', {}, limitedCtx)).rejects.toThrow('Insufficient permissions');
    });
});

#!/usr/bin/env node
/** MCP server that exposes Smooth tools to OpenCode workers */

import { ToolRegistry } from './registry.js';
import { artifactWriteTool } from './tools/artifact.js';
import { beadsContextTool } from './tools/beads-context.js';
import { beadsMessageTool } from './tools/beads-message.js';
import { progressAppendTool } from './tools/progress.js';
import { reviewRequestTool } from './tools/review-request.js';
import { spawnSubtaskTool } from './tools/spawn-subtask.js';
import { workflowTransitionTool } from './tools/workflow.js';
import type { ToolContext } from './types.js';
import type { ToolPermission } from '@smooai/smooth-shared/worker-types';

// Build tool context from environment
const ctx: ToolContext = {
    beadId: process.env.BEAD_ID ?? '',
    workerId: process.env.WORKER_ID ?? '',
    runId: process.env.RUN_ID ?? '',
    leaderUrl: process.env.LEADER_URL ?? 'http://leader:4400',
    permissions: (process.env.PERMISSIONS?.split(',') ?? [
        'beads:read', 'beads:write', 'beads:message', 'fs:read', 'fs:write',
    ]) as ToolPermission[],
};

// Register all tools
const registry = new ToolRegistry();
registry.register(beadsContextTool);
registry.register(beadsMessageTool);
registry.register(progressAppendTool);
registry.register(artifactWriteTool);
registry.register(workflowTransitionTool);
registry.register(spawnSubtaskTool);
registry.register(reviewRequestTool);

// Get available tools based on permissions
const available = registry.getAvailable(ctx.permissions);

console.log(`[mcp-server] Starting with ${available.length} tools for Smooth Operator ${ctx.workerId}`);
console.log(`[mcp-server] Bead: ${ctx.beadId}, Leader: ${ctx.leaderUrl}`);
console.log(`[mcp-server] Available tools: ${available.map((t) => t.name).join(', ')}`);

/**
 * MCP Protocol: stdio-based JSON-RPC
 *
 * The MCP server communicates via stdin/stdout using JSON-RPC 2.0.
 * OpenCode connects to this via mcpServers config.
 *
 * Supported methods:
 * - initialize: handshake
 * - tools/list: list available tools
 * - tools/call: execute a tool
 */

interface JsonRpcRequest {
    jsonrpc: '2.0';
    id: number | string;
    method: string;
    params?: Record<string, unknown>;
}

interface JsonRpcResponse {
    jsonrpc: '2.0';
    id: number | string;
    result?: unknown;
    error?: { code: number; message: string };
}

function respond(id: number | string, result: unknown): void {
    const response: JsonRpcResponse = { jsonrpc: '2.0', id, result };
    process.stdout.write(JSON.stringify(response) + '\n');
}

function respondError(id: number | string, code: number, message: string): void {
    const response: JsonRpcResponse = { jsonrpc: '2.0', id, error: { code, message } };
    process.stdout.write(JSON.stringify(response) + '\n');
}

async function handleRequest(req: JsonRpcRequest): Promise<void> {
    switch (req.method) {
        case 'initialize':
            respond(req.id, {
                protocolVersion: '2024-11-05',
                capabilities: { tools: {} },
                serverInfo: { name: 'smooth-tools', version: '0.1.0' },
            });
            break;

        case 'tools/list':
            respond(req.id, {
                tools: available.map((tool) => ({
                    name: tool.name,
                    description: tool.description,
                    inputSchema: {
                        type: 'object',
                        // Simplified schema — full Zod-to-JSON-Schema conversion would go here
                        properties: {},
                    },
                })),
            });
            break;

        case 'tools/call': {
            const { name, arguments: args } = req.params as { name: string; arguments: unknown };
            try {
                const result = await registry.execute(name, args, ctx);
                respond(req.id, { content: [{ type: 'text', text: JSON.stringify(result) }] });
            } catch (error) {
                const msg = error instanceof Error ? error.message : String(error);
                respond(req.id, { content: [{ type: 'text', text: `Error: ${msg}` }], isError: true });
            }
            break;
        }

        default:
            respondError(req.id, -32601, `Method not found: ${req.method}`);
    }
}

// Read JSON-RPC messages from stdin
let buffer = '';
process.stdin.setEncoding('utf8');
process.stdin.on('data', (chunk: string) => {
    buffer += chunk;
    const lines = buffer.split('\n');
    buffer = lines.pop() ?? '';

    for (const line of lines) {
        if (!line.trim()) continue;
        try {
            const req = JSON.parse(line) as JsonRpcRequest;
            handleRequest(req).catch((err) => {
                respondError(req.id, -32603, String(err));
            });
        } catch {
            // Ignore malformed JSON
        }
    }
});

process.stdin.on('end', () => {
    console.log('[mcp-server] stdin closed, shutting down');
    process.exit(0);
});

console.log('[mcp-server] Ready');

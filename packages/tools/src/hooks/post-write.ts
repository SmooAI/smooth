/** Post-write hook — tracks file changes for audit and review */

import { audit } from '@smooai/smooth-shared/audit-log';

import type { Hook } from './types.js';

/** Tracks files modified by operators during execution */
const changedFiles = new Map<string, Set<string>>(); // beadId → set of file paths

export function getChangedFiles(beadId: string): string[] {
    return Array.from(changedFiles.get(beadId) ?? []);
}

export function clearChangedFiles(beadId: string): void {
    changedFiles.delete(beadId);
}

/** Post-write hook: tracks which files were modified */
export const postWriteHook: Hook = {
    name: 'post-write-tracker',
    event: 'post-tool',
    tools: ['artifact_write'],
    handler: async (ctx) => {
        const input = ctx.input as Record<string, unknown>;
        const filePath = (input.path ?? input.name ?? 'unknown') as string;
        const beadId = ctx.toolContext.beadId;

        // Track the change
        if (!changedFiles.has(beadId)) {
            changedFiles.set(beadId, new Set());
        }
        changedFiles.get(beadId)!.add(filePath);

        // Audit log
        audit({
            actor: ctx.toolContext.workerId,
            action: 'tool_result',
            target: filePath,
            beadId,
            metadata: { tool: ctx.toolName, filesChanged: changedFiles.get(beadId)!.size },
        });

        return { allow: true };
    },
};

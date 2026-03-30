/** Dispatcher node — assigns beads to Smooth Operators via ExecutionBackend */

import { randomUUID } from 'node:crypto';

import type { OrchestratorStateType } from '../graph/state.js';
import { getBackend } from '../backend/registry.js';
import { updateBead } from '../beads/client.js';
import { sendMessage } from '../beads/messaging.js';
import { requestOperator } from '../sandbox/pool.js';

export async function dispatcherNode(state: OrchestratorStateType): Promise<Partial<OrchestratorStateType>> {
    const { readyBeads, activeWorkers } = state;
    const backend = getBackend();

    if (!backend.hasCapacity() || readyBeads.length === 0) {
        return { phase: 'monitoring' };
    }

    const newAssignments = { ...activeWorkers };

    for (const beadId of readyBeads) {
        if (!backend.hasCapacity()) break;

        const operatorId = `operator-${randomUUID().slice(0, 8)}`;

        // Update bead status and label with operator assignment
        await updateBead(beadId, {
            status: 'in_progress',
            addLabel: `worker:${operatorId}`,
        });
        await updateBead(beadId, { addLabel: 'phase:assess' });

        // Send assignment message
        await sendMessage(beadId, 'leader→worker', `Assigned to Smooth Operator ${operatorId}. Begin assessment.`, 'leader');

        // Create sandbox via backend-agnostic pool
        const handle = await requestOperator({
            beadId,
            operatorId,
            workspacePath: '/workspace',
            permissions: ['beads:read', 'beads:write', 'beads:message', 'fs:read', 'fs:write', 'exec:test'],
            phase: 'assess',
        });

        if (handle) {
            console.log(`[dispatcher] Spawned Smooth Operator ${operatorId} (sandbox ${handle.sandboxId}) for bead ${beadId}`);
        } else {
            console.log(`[dispatcher] Queued Smooth Operator ${operatorId} for bead ${beadId} (at capacity)`);
        }

        newAssignments[beadId] = operatorId;
    }

    return {
        activeWorkers: newAssignments,
        phase: 'monitoring',
    };
}

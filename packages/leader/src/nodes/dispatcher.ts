/** Dispatcher node — assigns beads to Smooth Operators via sandbox manager */

import { randomUUID } from 'node:crypto';

import type { OrchestratorStateType } from '../graph/state.js';
import { updateBead } from '../beads/client.js';
import { sendMessage } from '../beads/messaging.js';
import { hasCapacity } from '../sandbox/manager.js';
import { requestOperator } from '../sandbox/pool.js';

export async function dispatcherNode(state: OrchestratorStateType): Promise<Partial<OrchestratorStateType>> {
    const { readyBeads, activeWorkers } = state;

    if (!hasCapacity() || readyBeads.length === 0) {
        return { phase: 'monitoring' };
    }

    const newAssignments = { ...activeWorkers };

    for (const beadId of readyBeads) {
        if (!hasCapacity()) break;

        const operatorId = `operator-${randomUUID().slice(0, 8)}`;

        // Update bead status and label with operator assignment
        await updateBead(beadId, {
            status: 'in_progress',
            addLabel: `worker:${operatorId}`,
        });
        await updateBead(beadId, { addLabel: 'phase:assess' });

        // Send assignment message
        await sendMessage(beadId, 'leader→worker', `Assigned to Smooth Operator ${operatorId}. Begin assessment.`, 'leader');

        // Spawn Smooth Operator container via sandbox manager
        const operator = await requestOperator({
            beadId,
            operatorId,
            workspacePath: '/workspace', // Determined by workflow context
            permissions: ['beads:read', 'beads:write', 'beads:message', 'fs:read', 'fs:write', 'exec:test'],
            phase: 'assess',
        });

        if (operator) {
            newAssignments[beadId] = operatorId;
            console.log(`[dispatcher] Spawned Smooth Operator ${operatorId} (container ${operator.containerId}) for bead ${beadId}`);
        } else {
            console.log(`[dispatcher] Queued Smooth Operator ${operatorId} for bead ${beadId} (at capacity)`);
            newAssignments[beadId] = operatorId;
        }
    }

    return {
        activeWorkers: newAssignments,
        phase: 'monitoring',
    };
}

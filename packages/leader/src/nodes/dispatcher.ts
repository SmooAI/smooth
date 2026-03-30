/** Dispatcher node — assigns beads to Smooth Operators (workers) */

import { randomUUID } from 'node:crypto';

import type { OrchestratorStateType } from '../graph/state.js';
import { updateBead } from '../beads/client.js';
import { sendMessage } from '../beads/messaging.js';

/** Maximum concurrent Smooth Operators */
const MAX_OPERATORS = 3;

export async function dispatcherNode(state: OrchestratorStateType): Promise<Partial<OrchestratorStateType>> {
    const { readyBeads, activeWorkers } = state;

    const currentOperatorCount = Object.keys(activeWorkers).length;
    const slotsAvailable = MAX_OPERATORS - currentOperatorCount;

    if (slotsAvailable <= 0 || readyBeads.length === 0) {
        return { phase: 'monitoring' };
    }

    const newAssignments = { ...activeWorkers };
    const toDispatch = readyBeads.slice(0, slotsAvailable);

    for (const beadId of toDispatch) {
        const operatorId = `operator-${randomUUID().slice(0, 8)}`;

        // Update bead status and label with operator assignment
        await updateBead(beadId, {
            status: 'in_progress',
            addLabel: `worker:${operatorId}`,
        });
        await updateBead(beadId, { addLabel: 'phase:assess' });

        // Send assignment message
        await sendMessage(beadId, 'leader→worker', `Assigned to Smooth Operator ${operatorId}. Begin assessment.`, 'leader');

        newAssignments[beadId] = operatorId;

        // TODO: Actually spawn Docker container with OpenCode
        // This will be implemented in Phase 2 (sandbox manager)
        console.log(`[dispatcher] Assigned bead ${beadId} to Smooth Operator ${operatorId}`);
    }

    return {
        activeWorkers: newAssignments,
        phase: 'monitoring',
    };
}

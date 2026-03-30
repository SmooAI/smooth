/** Aggregator node — collects Smooth Operator updates and determines next actions */

import type { OrchestratorStateType } from '../graph/state.js';
import { listBeads } from '../beads/client.js';
import { readMessages } from '../beads/messaging.js';

export async function aggregatorNode(state: OrchestratorStateType): Promise<Partial<OrchestratorStateType>> {
    const { activeWorkers } = state;
    const completedBeads: string[] = [];
    const updatedWorkers = { ...activeWorkers };

    for (const [beadId, operatorId] of Object.entries(activeWorkers)) {
        // Check for completion messages from Smooth Operators
        const messages = await readMessages(beadId, 'worker→leader');
        const latestMessage = messages.at(-1);

        if (latestMessage?.content.toLowerCase().includes('finalized') || latestMessage?.content.toLowerCase().includes('completed')) {
            completedBeads.push(beadId);
            delete updatedWorkers[beadId];
            console.log(`[aggregator] Smooth Operator ${operatorId} completed bead ${beadId}`);
        }
    }

    // Check for newly blocked beads
    const blockedBeads = await listBeads({ status: 'blocked' });
    for (const bead of blockedBeads) {
        if (updatedWorkers[bead.id]) {
            console.log(`[aggregator] Bead ${bead.id} is blocked, releasing Smooth Operator ${updatedWorkers[bead.id]}`);
            delete updatedWorkers[bead.id];
        }
    }

    return {
        activeWorkers: updatedWorkers,
        completedBeads,
        phase: 'scheduling', // Loop back to scheduler
    };
}

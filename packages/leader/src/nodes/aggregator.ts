/** Aggregator node — collects Smooth Operator updates and determines next actions */

import type { OrchestratorStateType } from '../graph/state.js';

import { getBackend } from '../backend/registry.js';
import { listBeads } from '../beads/client.js';
import { readMessages } from '../beads/messaging.js';

export async function aggregatorNode(state: OrchestratorStateType): Promise<Partial<OrchestratorStateType>> {
    const { activeWorkers } = state;
    const backend = getBackend();
    const completedBeads: string[] = [];
    const updatedWorkers = { ...activeWorkers };

    for (const [beadId, operatorId] of Object.entries(activeWorkers)) {
        // Check sandbox status via backend
        const status = await backend.getSandboxStatus(operatorId);

        if (!status.running) {
            // Sandbox exited — collect artifacts and mark complete
            await backend.collectArtifacts(operatorId);
            completedBeads.push(beadId);
            delete updatedWorkers[beadId];
            console.log(`[aggregator] Smooth Operator ${operatorId} sandbox exited for bead ${beadId}`);
            continue;
        }

        // Also check for completion messages from Smooth Operators
        const messages = await readMessages(beadId, 'worker→leader');
        const latestMessage = messages.at(-1);

        if (latestMessage?.content.toLowerCase().includes('finalized') || latestMessage?.content.toLowerCase().includes('completed')) {
            completedBeads.push(beadId);
            delete updatedWorkers[beadId];
            await backend.destroySandbox(operatorId);
            console.log(`[aggregator] Smooth Operator ${operatorId} completed bead ${beadId}`);
        }
    }

    // Check for newly blocked beads
    const blockedBeads = await listBeads({ status: 'blocked' });
    for (const bead of blockedBeads) {
        if (updatedWorkers[bead.id]) {
            const operatorId = updatedWorkers[bead.id];
            console.log(`[aggregator] Bead ${bead.id} is blocked, releasing Smooth Operator ${operatorId}`);
            await backend.destroySandbox(operatorId);
            delete updatedWorkers[bead.id];
        }
    }

    return {
        activeWorkers: updatedWorkers,
        completedBeads,
        phase: 'scheduling',
    };
}

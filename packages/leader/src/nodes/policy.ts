/** Policy node — gates actions requiring human approval */

import type { OrchestratorStateType } from '../graph/state.js';

import { sendMessage } from '../beads/messaging.js';

/** Actions that require human approval before proceeding */
const GATED_ACTIONS = ['deploy', 'delete', 'merge', 'publish'];

export async function policyNode(state: OrchestratorStateType): Promise<Partial<OrchestratorStateType>> {
    const { pendingMessages } = state;

    for (const msg of pendingMessages) {
        const needsApproval = GATED_ACTIONS.some((action) => msg.content.toLowerCase().includes(action));

        if (needsApproval) {
            await sendMessage(msg.beadId, 'leader→human', `Approval required: ${msg.content}`, 'leader');
            console.log(`[policy] Gated action on bead ${msg.beadId}, requesting human approval`);
        }
    }

    return {
        pendingMessages: [],
    };
}

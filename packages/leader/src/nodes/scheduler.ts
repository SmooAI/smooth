/** Scheduler node — evaluates ready beads and prioritizes work */

import type { OrchestratorStateType } from '../graph/state.js';
import { getReady, listBeads } from '../beads/client.js';

export async function schedulerNode(state: OrchestratorStateType): Promise<Partial<OrchestratorStateType>> {
    try {
        // Get beads ready for work (open, no blockers)
        const ready = await getReady();

        // Also check for pending reviews
        const reviewBeads = await listBeads({ label: 'review:pending' });

        // Sort by priority (0 = highest)
        const sorted = ready.sort((a, b) => a.priority - b.priority);
        const readyIds = sorted.map((b) => b.id);
        const reviewIds = reviewBeads.map((b) => b.id);

        return {
            readyBeads: readyIds,
            pendingReviews: reviewIds,
            phase: readyIds.length > 0 || reviewIds.length > 0 ? 'dispatching' : 'idle',
        };
    } catch (error) {
        return {
            error: `Scheduler error: ${error instanceof Error ? error.message : String(error)}`,
            phase: 'idle',
        };
    }
}

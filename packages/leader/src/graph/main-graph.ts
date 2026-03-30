/** Main orchestration graph — the leader's control loop */

import { END, START, StateGraph } from '@langchain/langgraph';

import { aggregatorNode } from '../nodes/aggregator.js';
import { dispatcherNode } from '../nodes/dispatcher.js';
import { policyNode } from '../nodes/policy.js';
import { schedulerNode } from '../nodes/scheduler.js';
import { OrchestratorState, type OrchestratorStateType } from './state.js';

/**
 * Build the main orchestration graph.
 *
 * Flow:
 *   START → scheduler → router → dispatcher → monitor → aggregator → scheduler (loop)
 *                     ↘ policy (if gated actions)
 *                     ↘ END (if idle)
 */
export function buildMainGraph() {
    const graph = new StateGraph(OrchestratorState)
        .addNode('scheduler', schedulerNode)
        .addNode('dispatcher', dispatcherNode)
        .addNode('aggregator', aggregatorNode)
        .addNode('policy', policyNode)

        // START → scheduler
        .addEdge(START, 'scheduler')

        // scheduler → conditional routing
        .addConditionalEdges('scheduler', routeFromScheduler)

        // dispatcher → aggregator
        .addEdge('dispatcher', 'aggregator')

        // aggregator → scheduler (loop)
        .addEdge('aggregator', 'scheduler')

        // policy → scheduler
        .addEdge('policy', 'scheduler');

    return graph;
}

/** Route from scheduler based on phase */
function routeFromScheduler(state: OrchestratorStateType): string {
    if (state.error) return END;

    if (state.pendingMessages.length > 0) return 'policy';

    if (state.phase === 'dispatching') return 'dispatcher';

    // Idle — nothing to do
    return END;
}

/** Create a compiled graph with optional checkpointing */
export function createOrchestrator(checkpointer?: unknown) {
    const graph = buildMainGraph();
    return graph.compile({
        checkpointer: checkpointer as never,
    });
}

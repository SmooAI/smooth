/** Smooth Operator pool management — warm pool and scheduling */

import type { ToolPermission, WorkerPhase } from '@smooth/shared/worker-types';

import { destroyOperator, enforceTimeouts, getActiveCount, getActiveOperators, hasCapacity, healthCheck, spawnOperator, type OperatorConfig, type RunningOperator } from './manager.js';

export interface OperatorRequest {
    beadId: string;
    operatorId: string;
    workspacePath: string;
    permissions: ToolPermission[];
    systemPrompt?: string;
    model?: string;
    phase: WorkerPhase;
}

/** Queue of pending operator requests when at capacity */
const requestQueue: OperatorRequest[] = [];

/** Request a Smooth Operator — spawns immediately or queues if at capacity */
export async function requestOperator(req: OperatorRequest): Promise<RunningOperator | null> {
    if (hasCapacity()) {
        return spawnOperator({
            operatorId: req.operatorId,
            beadId: req.beadId,
            workspacePath: req.workspacePath,
            permissions: req.permissions,
            systemPrompt: req.systemPrompt,
            model: req.model,
            phase: req.phase,
        });
    }

    // Queue the request
    requestQueue.push(req);
    console.log(`[pool] Smooth Operator request queued for bead ${req.beadId} (queue size: ${requestQueue.length})`);
    return null;
}

/** Process queued requests when capacity frees up */
export async function processQueue(): Promise<RunningOperator[]> {
    const spawned: RunningOperator[] = [];

    while (hasCapacity() && requestQueue.length > 0) {
        const req = requestQueue.shift()!;
        try {
            const operator = await spawnOperator({
                operatorId: req.operatorId,
                beadId: req.beadId,
                workspacePath: req.workspacePath,
                permissions: req.permissions,
                systemPrompt: req.systemPrompt,
                model: req.model,
                phase: req.phase,
            });
            spawned.push(operator);
        } catch (error) {
            console.error(`[pool] Failed to spawn queued operator for bead ${req.beadId}:`, error);
        }
    }

    return spawned;
}

/** Run a maintenance cycle: enforce timeouts, clean unhealthy, process queue */
export async function maintenanceCycle(): Promise<{
    timedOut: string[];
    cleaned: string[];
    spawned: RunningOperator[];
}> {
    // Enforce timeouts
    const timedOut = await enforceTimeouts();

    // Health check
    const { unhealthy } = await healthCheck();
    for (const id of unhealthy) {
        await destroyOperator(id);
    }

    // Process queue with freed capacity
    const spawned = await processQueue();

    return { timedOut, cleaned: unhealthy, spawned };
}

/** Get pool status */
export function getPoolStatus() {
    return {
        active: getActiveCount(),
        queued: requestQueue.length,
        operators: getActiveOperators(),
    };
}

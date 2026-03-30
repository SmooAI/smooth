/** Smooth Operator pool management — backend-agnostic queue and scheduling */

import type { ToolPermission, WorkerPhase } from '@smooth/shared/worker-types';
import { PHASE_TIMEOUTS } from '@smooth/shared/worker-types';

import { getBackend } from '../backend/registry.js';
import type { SandboxHandle } from '../backend/types.js';

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

/** Request a Smooth Operator — creates sandbox immediately or queues if at capacity */
export async function requestOperator(req: OperatorRequest): Promise<SandboxHandle | null> {
    const backend = getBackend();

    if (backend.hasCapacity()) {
        return backend.createSandbox({
            operatorId: req.operatorId,
            beadId: req.beadId,
            workspacePath: req.workspacePath,
            permissions: req.permissions,
            systemPrompt: req.systemPrompt,
            model: req.model,
            phase: req.phase,
            timeoutSeconds: PHASE_TIMEOUTS[req.phase],
        });
    }

    // Queue the request
    requestQueue.push(req);
    console.log(`[pool] Smooth Operator request queued for bead ${req.beadId} (queue size: ${requestQueue.length})`);
    return null;
}

/** Process queued requests when capacity frees up */
export async function processQueue(): Promise<SandboxHandle[]> {
    const backend = getBackend();
    const spawned: SandboxHandle[] = [];

    while (backend.hasCapacity() && requestQueue.length > 0) {
        const req = requestQueue.shift()!;
        try {
            const handle = await backend.createSandbox({
                operatorId: req.operatorId,
                beadId: req.beadId,
                workspacePath: req.workspacePath,
                permissions: req.permissions,
                systemPrompt: req.systemPrompt,
                model: req.model,
                phase: req.phase,
                timeoutSeconds: PHASE_TIMEOUTS[req.phase],
            });
            spawned.push(handle);
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
    spawned: SandboxHandle[];
}> {
    const backend = getBackend();

    const timedOut = await backend.enforceTimeouts();

    const { unhealthy } = await backend.healthCheck();
    for (const id of unhealthy) {
        await backend.destroySandbox(id);
    }

    const spawned = await processQueue();

    return { timedOut, cleaned: unhealthy, spawned };
}

/** Get pool status */
export async function getPoolStatus() {
    const backend = getBackend();
    const sandboxes = await backend.listSandboxes();

    return {
        active: backend.activeCount(),
        maxConcurrency: backend.maxConcurrency(),
        queued: requestQueue.length,
        backend: backend.name,
        sandboxes,
    };
}

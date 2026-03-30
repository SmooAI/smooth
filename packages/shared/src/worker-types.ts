/** Worker lifecycle types */

export type WorkerPhase = 'assess' | 'plan' | 'orchestrate' | 'execute' | 'finalize';

export type WorkerStatus = 'pending' | 'running' | 'completed' | 'failed' | 'timeout';

export interface Worker {
    id: string;
    beadId: string;
    sandboxId: string;
    backendType: string;
    phase: WorkerPhase;
    status: WorkerStatus;
    startedAt: string;
    completedAt?: string;
    metadata: Record<string, unknown>;
}

export interface WorkerDetail extends Worker {
    logs: string[];
    artifacts: string[];
    progressUpdates: string[];
}

export interface WorkerDispatch {
    beadId: string;
    permissions: ToolPermission[];
    systemPrompt?: string;
    model?: string;
    timeout?: number; // seconds
}

export type ToolPermission =
    | 'beads:read'
    | 'beads:write'
    | 'beads:message'
    | 'fs:read'
    | 'fs:write'
    | 'net:internal'
    | 'net:external'
    | 'exec:test'
    | 'smoo:read'
    | 'smoo:write';

/** Default phase timeouts in seconds */
export const PHASE_TIMEOUTS: Record<WorkerPhase, number> = {
    assess: 30 * 60, // 30 minutes
    plan: 10 * 60, // 10 minutes
    orchestrate: 15 * 60, // 15 minutes
    execute: 60 * 60, // 60 minutes
    finalize: 15 * 60, // 15 minutes
};

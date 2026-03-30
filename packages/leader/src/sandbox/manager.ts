/** Docker sandbox manager — creates, monitors, and destroys Smooth Operator containers */

import { execFile } from 'node:child_process';
import { randomUUID } from 'node:crypto';
import { promisify } from 'node:util';

import type { ToolPermission, WorkerPhase } from '@smooth/shared/worker-types';
import { PHASE_TIMEOUTS } from '@smooth/shared/worker-types';

const exec = promisify(execFile);

export interface OperatorConfig {
    operatorId: string;
    beadId: string;
    workspacePath: string;
    permissions: ToolPermission[];
    systemPrompt?: string;
    model?: string;
    phase: WorkerPhase;
}

export interface RunningOperator {
    operatorId: string;
    containerId: string;
    beadId: string;
    phase: WorkerPhase;
    startedAt: Date;
    timeoutAt: Date;
}

const WORKER_IMAGE = process.env.SMOOTH_WORKER_IMAGE ?? 'smooth-operator:latest';
const NETWORK = process.env.SMOOTH_NETWORK ?? 'smooth-net';
const LEADER_URL = process.env.LEADER_INTERNAL_URL ?? 'http://leader:4400';
const MAX_OPERATORS = parseInt(process.env.MAX_OPERATORS ?? '3', 10);

/** Active Smooth Operators indexed by operatorId */
const activeOperators = new Map<string, RunningOperator>();

export function getActiveOperators(): RunningOperator[] {
    return Array.from(activeOperators.values());
}

export function getActiveCount(): number {
    return activeOperators.size;
}

export function hasCapacity(): boolean {
    return activeOperators.size < MAX_OPERATORS;
}

/** Spawn a new Smooth Operator container */
export async function spawnOperator(config: OperatorConfig): Promise<RunningOperator> {
    if (!hasCapacity()) {
        throw new Error(`Cannot spawn Smooth Operator: at capacity (${MAX_OPERATORS})`);
    }

    const containerName = `smooth-operator-${config.operatorId}`;
    const timeout = PHASE_TIMEOUTS[config.phase];
    const now = new Date();

    // Build OpenCode config for injection
    const openCodeConfig = buildOpenCodeConfig(config);

    const args = [
        'run', '-d',
        '--name', containerName,
        '--network', NETWORK,
        // Resource limits
        '--cpus', '2',
        '--memory', '4g',
        // Workspace mount
        '-v', `${config.workspacePath}:/workspace`,
        // Environment
        '-e', `LEADER_URL=${LEADER_URL}`,
        '-e', `BEAD_ID=${config.beadId}`,
        '-e', `WORKER_ID=${config.operatorId}`,
        '-e', `RUN_ID=${randomUUID()}`,
        '-e', `SMOOTH_PHASE=${config.phase}`,
        '-e', `OPENCODE_CONFIG=${JSON.stringify(openCodeConfig)}`,
        // Labels for identification
        '--label', `smooth.operator=${config.operatorId}`,
        '--label', `smooth.bead=${config.beadId}`,
        '--label', `smooth.phase=${config.phase}`,
        // Image
        WORKER_IMAGE,
    ];

    const { stdout } = await exec('docker', args);
    const containerId = stdout.trim().slice(0, 12);

    const operator: RunningOperator = {
        operatorId: config.operatorId,
        containerId,
        beadId: config.beadId,
        phase: config.phase,
        startedAt: now,
        timeoutAt: new Date(now.getTime() + timeout * 1000),
    };

    activeOperators.set(config.operatorId, operator);
    console.log(`[sandbox] Spawned Smooth Operator ${config.operatorId} (container ${containerId}) for bead ${config.beadId}`);

    return operator;
}

/** Check if a Smooth Operator container is still running */
export async function isOperatorRunning(operatorId: string): Promise<boolean> {
    const operator = activeOperators.get(operatorId);
    if (!operator) return false;

    try {
        const { stdout } = await exec('docker', ['inspect', '--format', '{{.State.Running}}', operator.containerId]);
        return stdout.trim() === 'true';
    } catch {
        return false;
    }
}

/** Get logs from a Smooth Operator container */
export async function getOperatorLogs(operatorId: string, tail = 100): Promise<string> {
    const operator = activeOperators.get(operatorId);
    if (!operator) throw new Error(`Smooth Operator ${operatorId} not found`);

    const { stdout } = await exec('docker', ['logs', '--tail', String(tail), operator.containerId]);
    return stdout;
}

/** Stop and remove a Smooth Operator container */
export async function destroyOperator(operatorId: string): Promise<void> {
    const operator = activeOperators.get(operatorId);
    if (!operator) return;

    try {
        await exec('docker', ['stop', '-t', '10', operator.containerId]);
    } catch {
        // Container may have already stopped
    }

    try {
        await exec('docker', ['rm', '-f', operator.containerId]);
    } catch {
        // Container may have already been removed
    }

    activeOperators.delete(operatorId);
    console.log(`[sandbox] Destroyed Smooth Operator ${operatorId}`);
}

/** Check for timed-out operators and destroy them */
export async function enforceTimeouts(): Promise<string[]> {
    const now = new Date();
    const timedOut: string[] = [];

    for (const [id, operator] of activeOperators) {
        if (now > operator.timeoutAt) {
            console.log(`[sandbox] Smooth Operator ${id} timed out (phase: ${operator.phase})`);
            await destroyOperator(id);
            timedOut.push(id);
        }
    }

    return timedOut;
}

/** Collect output/artifacts from a completed operator's workspace */
export async function collectArtifacts(operatorId: string): Promise<string[]> {
    const operator = activeOperators.get(operatorId);
    if (!operator) return [];

    try {
        // List files modified in the workspace
        const { stdout } = await exec('docker', [
            'exec', operator.containerId,
            'git', '-C', '/workspace', 'diff', '--name-only',
        ]);
        return stdout.trim().split('\n').filter(Boolean);
    } catch {
        return [];
    }
}

/** Health check all active operators */
export async function healthCheck(): Promise<{ healthy: string[]; unhealthy: string[] }> {
    const healthy: string[] = [];
    const unhealthy: string[] = [];

    for (const [id] of activeOperators) {
        if (await isOperatorRunning(id)) {
            healthy.push(id);
        } else {
            unhealthy.push(id);
        }
    }

    return { healthy, unhealthy };
}

// ── Helpers ─────────────────────────────────────────────────

function buildOpenCodeConfig(config: OperatorConfig): Record<string, unknown> {
    return {
        providers: {
            'opencode-zen': {
                disabled: false,
            },
        },
        agents: {
            coder: {
                model: config.model ?? 'opencode/zen',
                maxTokens: 8000,
            },
        },
        mcpServers: {
            'smooth-tools': {
                command: 'smooth-tools-server',
                args: ['--socket', '/tmp/smooth-tools.sock'],
                env: {
                    LEADER_URL,
                    BEAD_ID: config.beadId,
                    WORKER_ID: config.operatorId,
                    PERMISSIONS: config.permissions.join(','),
                },
            },
        },
        autoCompact: true,
    };
}

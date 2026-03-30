/** LocalMicrosandboxBackend — Microsandbox microVM execution backend for local development */

import { execFile } from 'node:child_process';
import { randomUUID } from 'node:crypto';
import { promisify } from 'node:util';

import type { WorkerPhase } from '@smooai/smooth-shared/worker-types';

import { createAuditLogger } from '@smooai/smooth-shared/audit-log';
import { PHASE_TIMEOUTS } from '@smooai/smooth-shared/worker-types';

import type { ExecutionBackend, PromptResult, SandboxConfig, SandboxHandle, SandboxStatus } from './types.js';

import { getEventStream } from './registry.js';

const audit = createAuditLogger('leader');

const exec = promisify(execFile);

interface ManagedSandbox {
    handle: SandboxHandle;
    /** Port the OpenCode API is reachable at on the host */
    hostPort: number;
}

const LEADER_URL = process.env.LEADER_URL ?? 'http://host.containers.internal:4400';
const MAX_OPERATORS = parseInt(process.env.MAX_OPERATORS ?? '3', 10);
const BASE_PORT = 14096; // Host ports for sandbox OpenCode APIs

export class LocalMicrosandboxBackend implements ExecutionBackend {
    readonly name = 'local-microsandbox';
    private sandboxes = new Map<string, ManagedSandbox>();
    private nextPort = BASE_PORT;

    async initialize(): Promise<void> {
        // Validate microsandbox server is running
        try {
            await exec('msb', ['server', 'status']);
            console.log('[microsandbox] Server is running');
        } catch {
            console.log('[microsandbox] Server not running, attempting to start...');
            try {
                await exec('msb', ['server', 'start', '--dev']);
                // Wait a moment for the server to be ready
                await new Promise((resolve) => setTimeout(resolve, 2000));
                console.log('[microsandbox] Server started');
            } catch (error) {
                throw new Error(
                    `Microsandbox server failed to start. Install: curl -sSL https://get.microsandbox.dev | sh\nError: ${(error as Error).message}`,
                    { cause: error },
                );
            }
        }
    }

    async shutdown(): Promise<void> {
        // Destroy all active sandboxes
        const ids = Array.from(this.sandboxes.keys());
        for (const id of ids) {
            await this.destroySandbox(id);
        }
        console.log('[microsandbox] All sandboxes cleaned up');
    }

    async createSandbox(config: SandboxConfig): Promise<SandboxHandle> {
        if (!this.hasCapacity()) {
            throw new Error(`Cannot create sandbox: at capacity (${MAX_OPERATORS})`);
        }

        const sandboxName = `smooth-operator-${config.operatorId}`;
        const hostPort = this.nextPort++;
        const timeout = config.timeoutSeconds ?? PHASE_TIMEOUTS[config.phase];
        const now = new Date();

        // Build the OpenCode config for the sandbox
        const openCodeConfig = this.buildOpenCodeConfig(config);

        // Create and start the microsandbox
        // msb supports OCI images, directory mounts, resource limits
        const msbArgs = [
            'run',
            '--name',
            sandboxName,
            '--image',
            process.env.SMOOTH_WORKER_IMAGE ?? 'smooth-operator:latest',
            '--memory',
            String(config.resourceLimits?.memoryMb ?? 4096),
            '--cpus',
            String(config.resourceLimits?.cpus ?? 2),
            '--port',
            `${hostPort}:4096`,
            '--mount',
            `${config.workspacePath}:/workspace`,
            '--env',
            `LEADER_URL=${LEADER_URL}`,
            '--env',
            `BEAD_ID=${config.beadId}`,
            '--env',
            `WORKER_ID=${config.operatorId}`,
            '--env',
            `RUN_ID=${randomUUID()}`,
            '--env',
            `SMOOTH_PHASE=${config.phase}`,
            '--env',
            `OPENCODE_CONFIG=${JSON.stringify(openCodeConfig)}`,
        ];

        // Add custom env vars
        if (config.env) {
            for (const [key, value] of Object.entries(config.env)) {
                msbArgs.push('--env', `${key}=${value}`);
            }
        }

        try {
            await exec('msb', msbArgs);
        } catch (error) {
            throw new Error(`Failed to create sandbox ${sandboxName}: ${(error as Error).message}`, { cause: error });
        }

        const handle: SandboxHandle = {
            sandboxId: config.operatorId,
            operatorId: config.operatorId,
            beadId: config.beadId,
            backendRef: { microsandboxName: sandboxName, hostPort },
            createdAt: now,
            timeoutAt: new Date(now.getTime() + timeout * 1000),
        };

        this.sandboxes.set(config.operatorId, { handle, hostPort });

        // Wait for OpenCode to be ready
        await this.waitForReady(config.operatorId, hostPort);

        getEventStream().emit({
            type: 'sandbox_created',
            sandboxId: config.operatorId,
            operatorId: config.operatorId,
            beadId: config.beadId,
            data: { phase: config.phase, hostPort },
            timestamp: now,
        });

        audit.sandboxCreated(config.operatorId, { beadId: config.beadId, phase: config.phase, hostPort });
        console.log(`[microsandbox] Spawned Smooth Operator ${config.operatorId} on port ${hostPort} for bead ${config.beadId}`);
        return handle;
    }

    async destroySandbox(sandboxId: string): Promise<void> {
        const entry = this.sandboxes.get(sandboxId);
        if (!entry) return;

        const sandboxName = entry.handle.backendRef.microsandboxName as string;

        try {
            await exec('msb', ['stop', sandboxName]);
        } catch {
            // Sandbox may have already stopped
        }

        try {
            await exec('msb', ['rm', sandboxName]);
        } catch {
            // Sandbox may have already been removed
        }

        this.sandboxes.delete(sandboxId);

        getEventStream().emit({
            type: 'sandbox_destroyed',
            sandboxId,
            operatorId: sandboxId,
            beadId: entry.handle.beadId,
            data: {},
            timestamp: new Date(),
        });

        audit.sandboxDestroyed(sandboxId);
        console.log(`[microsandbox] Destroyed Smooth Operator ${sandboxId}`);
    }

    async getSandboxStatus(sandboxId: string): Promise<SandboxStatus> {
        const entry = this.sandboxes.get(sandboxId);
        if (!entry) {
            return { running: false, healthy: false, phase: 'assess', uptimeMs: 0 };
        }

        const running = await this.isSandboxRunning(sandboxId);
        const healthy = running ? await this.checkSandboxHealth(entry.hostPort) : false;

        return {
            running,
            healthy,
            phase: (entry.handle.backendRef.phase as WorkerPhase) ?? 'assess',
            uptimeMs: Date.now() - entry.handle.createdAt.getTime(),
        };
    }

    async listSandboxes(): Promise<SandboxHandle[]> {
        return Array.from(this.sandboxes.values()).map((e) => e.handle);
    }

    async prompt(sandboxId: string, sessionId: string, text: string): Promise<PromptResult> {
        const entry = this.sandboxes.get(sandboxId);
        if (!entry) throw new Error(`Sandbox ${sandboxId} not found`);

        const baseUrl = `http://localhost:${entry.hostPort}`;
        const response = await fetch(`${baseUrl}/session/${sessionId}/prompt`, {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({ parts: [{ type: 'text', text }] }),
        });

        if (!response.ok) {
            const body = await response.text().catch(() => '');
            throw new Error(`Prompt failed: ${response.status} ${body}`);
        }

        return response.json() as Promise<PromptResult>;
    }

    async createSession(sandboxId: string, title: string): Promise<{ id: string; title: string }> {
        const entry = this.sandboxes.get(sandboxId);
        if (!entry) throw new Error(`Sandbox ${sandboxId} not found`);

        const baseUrl = `http://localhost:${entry.hostPort}`;
        const response = await fetch(`${baseUrl}/session`, {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({ title }),
        });

        if (!response.ok) {
            throw new Error(`Failed to create session: ${response.status}`);
        }

        return response.json() as Promise<{ id: string; title: string }>;
    }

    async abort(sandboxId: string, sessionId: string): Promise<void> {
        const entry = this.sandboxes.get(sandboxId);
        if (!entry) return;

        const baseUrl = `http://localhost:${entry.hostPort}`;
        await fetch(`${baseUrl}/session/${sessionId}/abort`, { method: 'POST' }).catch(() => {});
    }

    async getLogs(sandboxId: string, tail = 100): Promise<string> {
        const entry = this.sandboxes.get(sandboxId);
        if (!entry) return '';

        const sandboxName = entry.handle.backendRef.microsandboxName as string;
        try {
            const { stdout } = await exec('msb', ['logs', sandboxName, '--tail', String(tail)]);
            return stdout;
        } catch {
            return '';
        }
    }

    async healthCheck(): Promise<{ healthy: string[]; unhealthy: string[] }> {
        const healthy: string[] = [];
        const unhealthy: string[] = [];

        for (const [id, entry] of this.sandboxes) {
            if (await this.checkSandboxHealth(entry.hostPort)) {
                healthy.push(id);
            } else {
                unhealthy.push(id);
            }
        }

        return { healthy, unhealthy };
    }

    hasCapacity(): boolean {
        return this.sandboxes.size < MAX_OPERATORS;
    }

    activeCount(): number {
        return this.sandboxes.size;
    }

    maxConcurrency(): number {
        return MAX_OPERATORS;
    }

    async collectArtifacts(sandboxId: string): Promise<string[]> {
        const entry = this.sandboxes.get(sandboxId);
        if (!entry) return [];

        const sandboxName = entry.handle.backendRef.microsandboxName as string;
        try {
            const { stdout } = await exec('msb', ['exec', sandboxName, '--', 'git', '-C', '/workspace', 'diff', '--name-only']);
            return stdout.trim().split('\n').filter(Boolean);
        } catch {
            return [];
        }
    }

    async enforceTimeouts(): Promise<string[]> {
        const now = new Date();
        const timedOut: string[] = [];

        for (const [id, entry] of this.sandboxes) {
            if (now > entry.handle.timeoutAt) {
                console.log(`[microsandbox] Smooth Operator ${id} timed out`);
                getEventStream().emit({
                    type: 'timeout',
                    sandboxId: id,
                    operatorId: id,
                    beadId: entry.handle.beadId,
                    data: {},
                    timestamp: now,
                });
                await this.destroySandbox(id);
                timedOut.push(id);
            }
        }

        return timedOut;
    }

    async exec(sandboxId: string, command: string[]): Promise<{ stdout: string; stderr: string; exitCode: number }> {
        const entry = this.sandboxes.get(sandboxId);
        if (!entry) throw new Error(`Sandbox ${sandboxId} not found`);

        const sandboxName = entry.handle.backendRef.microsandboxName as string;
        try {
            const { stdout, stderr } = await exec('msb', ['exec', sandboxName, '--', ...command]);
            return { stdout, stderr: stderr ?? '', exitCode: 0 };
        } catch (error: any) {
            return {
                stdout: error.stdout ?? '',
                stderr: error.stderr ?? error.message ?? '',
                exitCode: error.code ?? 1,
            };
        }
    }

    async snapshotWorkspace(sandboxId: string): Promise<Buffer> {
        const entry = this.sandboxes.get(sandboxId);
        if (!entry) throw new Error(`Sandbox ${sandboxId} not found`);

        const sandboxName = entry.handle.backendRef.microsandboxName as string;
        // Create tarball of workspace inside sandbox, then read it out
        await exec('msb', ['exec', sandboxName, '--', 'tar', 'czf', '/tmp/workspace-snapshot.tar.gz', '-C', '/workspace', '.']);
        // Copy the tarball out of the sandbox
        const { stdout } = await exec('msb', ['exec', sandboxName, '--', 'cat', '/tmp/workspace-snapshot.tar.gz']);
        return Buffer.from(stdout, 'binary');
    }

    async restoreWorkspace(sandboxId: string, snapshot: Buffer): Promise<void> {
        const entry = this.sandboxes.get(sandboxId);
        if (!entry) throw new Error(`Sandbox ${sandboxId} not found`);

        const sandboxName = entry.handle.backendRef.microsandboxName as string;
        // Write snapshot into sandbox and extract
        const { writeFileSync, unlinkSync } = await import('node:fs');
        const tmpPath = `/tmp/restore-${sandboxId}.tar.gz`;
        writeFileSync(tmpPath, snapshot);
        // Copy into sandbox and extract
        await exec('msb', ['exec', sandboxName, '--', 'rm', '-rf', '/workspace/*']);
        await exec('msb', ['exec', sandboxName, '--', 'tar', 'xzf', '/tmp/workspace-snapshot.tar.gz', '-C', '/workspace']);
        unlinkSync(tmpPath);
    }

    // ── Private helpers ─────────────────────────────────────

    private async isSandboxRunning(sandboxId: string): Promise<boolean> {
        const entry = this.sandboxes.get(sandboxId);
        if (!entry) return false;

        const sandboxName = entry.handle.backendRef.microsandboxName as string;
        try {
            const { stdout } = await exec('msb', ['status', sandboxName]);
            return stdout.toLowerCase().includes('running');
        } catch {
            return false;
        }
    }

    private async checkSandboxHealth(hostPort: number): Promise<boolean> {
        try {
            const response = await fetch(`http://localhost:${hostPort}/health`, {
                signal: AbortSignal.timeout(3000),
            });
            return response.ok;
        } catch {
            return false;
        }
    }

    private async waitForReady(sandboxId: string, hostPort: number, maxRetries = 30, intervalMs = 2000): Promise<void> {
        for (let i = 0; i < maxRetries; i++) {
            if (await this.checkSandboxHealth(hostPort)) {
                console.log(`[microsandbox] Smooth Operator ${sandboxId} is ready`);
                return;
            }
            await new Promise((resolve) => setTimeout(resolve, intervalMs));
        }
        throw new Error(`Smooth Operator ${sandboxId} failed to become ready after ${maxRetries * intervalMs}ms`);
    }

    private buildOpenCodeConfig(config: SandboxConfig): Record<string, unknown> {
        return {
            providers: {
                'opencode-zen': { disabled: false },
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
}

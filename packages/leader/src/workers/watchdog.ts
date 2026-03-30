/** OperatorWatchdog — periodic health monitoring + stuck detection */

import { createAuditLogger } from '@smooai/smooth-shared/audit-log';

import { getBackend, getEventStream } from '../backend/registry.js';
import { getLastProgressTimestamp } from '../beads/messaging.js';
import { checkpointManager } from './checkpoint.js';

const audit = createAuditLogger('leader');

const POLL_INTERVAL_MS = 30_000;
const STUCK_THRESHOLD_MS = 5 * 60 * 1000; // 5 minutes without progress → nudge
const DEAD_THRESHOLD_MS = 15 * 60 * 1000; // 15 minutes without progress → kill

export class OperatorWatchdog {
    private intervalId: ReturnType<typeof setInterval> | null = null;

    start(intervalMs = POLL_INTERVAL_MS): void {
        if (this.intervalId) return;

        this.intervalId = setInterval(() => {
            this.patrol().catch((err) => {
                console.error('[watchdog] Patrol error:', err);
            });
        }, intervalMs);

        console.log(`[watchdog] Started (poll every ${intervalMs / 1000}s)`);
    }

    stop(): void {
        if (this.intervalId) {
            clearInterval(this.intervalId);
            this.intervalId = null;
            console.log('[watchdog] Stopped');
        }
    }

    async patrol(): Promise<void> {
        const backend = getBackend();
        const sandboxes = await backend.listSandboxes();
        const events = getEventStream();

        for (const sandbox of sandboxes) {
            // Check if sandbox is still running
            const status = await backend.getSandboxStatus(sandbox.sandboxId);

            if (!status.running) {
                // Sandbox died — attempt recovery
                console.log(`[watchdog] Sandbox ${sandbox.sandboxId} not running, attempting recovery`);
                events.emit({
                    type: 'watchdog_restart',
                    sandboxId: sandbox.sandboxId,
                    operatorId: sandbox.operatorId,
                    beadId: sandbox.beadId,
                    data: { reason: 'not_running' },
                    timestamp: new Date(),
                });
                audit.error(`Sandbox ${sandbox.sandboxId} found dead, triggering recovery`, { beadId: sandbox.beadId });
                continue;
            }

            if (!status.healthy) {
                // Sandbox is running but unhealthy
                console.log(`[watchdog] Sandbox ${sandbox.sandboxId} unhealthy`);
                continue;
            }

            // Check for stuck operators (no progress messages)
            const lastProgress = await getLastProgressTimestamp(sandbox.beadId);
            if (!lastProgress) continue;

            const silenceMs = Date.now() - lastProgress.getTime();

            if (silenceMs > DEAD_THRESHOLD_MS) {
                // Dead — kill and restart
                console.log(`[watchdog] Sandbox ${sandbox.sandboxId} dead (${Math.round(silenceMs / 60000)}m silent), killing`);
                events.emit({
                    type: 'watchdog_killed',
                    sandboxId: sandbox.sandboxId,
                    operatorId: sandbox.operatorId,
                    beadId: sandbox.beadId,
                    data: { silenceMs, threshold: DEAD_THRESHOLD_MS },
                    timestamp: new Date(),
                });
                audit.error(`Killed stuck sandbox ${sandbox.sandboxId} after ${Math.round(silenceMs / 60000)}m`, { beadId: sandbox.beadId });
                await backend.destroySandbox(sandbox.sandboxId);
            } else if (silenceMs > STUCK_THRESHOLD_MS) {
                // Stuck — send a nudge
                console.log(`[watchdog] Sandbox ${sandbox.sandboxId} stuck (${Math.round(silenceMs / 60000)}m silent), nudging`);
                events.emit({
                    type: 'watchdog_stuck',
                    sandboxId: sandbox.sandboxId,
                    operatorId: sandbox.operatorId,
                    beadId: sandbox.beadId,
                    data: { silenceMs, threshold: STUCK_THRESHOLD_MS },
                    timestamp: new Date(),
                });
            }
        }

        // Also enforce timeouts
        await backend.enforceTimeouts();
    }
}

export const watchdog = new OperatorWatchdog();

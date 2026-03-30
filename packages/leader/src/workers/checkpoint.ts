/** CheckpointManager — workspace snapshots for crash recovery */

import { createAuditLogger } from '@smooai/smooth-shared/audit-log';

import { LocalArtifactStore } from '../backend/local-artifact-store.js';
import { getBackend } from '../backend/registry.js';
import { getEventStream } from '../backend/registry.js';

const audit = createAuditLogger('leader');
const artifactStore = new LocalArtifactStore();

export class CheckpointManager {
    /** Save a workspace checkpoint for a sandbox */
    async saveCheckpoint(sandboxId: string, beadId: string, phase: string): Promise<string> {
        const backend = getBackend();
        const snapshot = await backend.snapshotWorkspace(sandboxId);
        const key = await artifactStore.put(beadId, `checkpoint-${phase}-${Date.now()}.tar.gz`, snapshot);

        audit.toolCall('checkpoint_save', { sandboxId, beadId, phase }, { key, size: snapshot.length });

        getEventStream().emit({
            type: 'checkpoint_saved',
            sandboxId,
            operatorId: sandboxId,
            beadId,
            data: { phase, key, size: snapshot.length },
            timestamp: new Date(),
        });

        return key;
    }

    /** Restore workspace from the latest checkpoint */
    async restoreCheckpoint(sandboxId: string, beadId: string): Promise<boolean> {
        const backend = getBackend();
        const keys = await artifactStore.list(beadId);
        const checkpoints = keys
            .filter((k) => k.includes('checkpoint-'))
            .sort()
            .reverse();

        if (checkpoints.length === 0) return false;

        const snapshot = await artifactStore.get(checkpoints[0]);
        await backend.restoreWorkspace(sandboxId, snapshot);

        audit.toolCall('checkpoint_restore', { sandboxId, beadId }, { key: checkpoints[0] });

        getEventStream().emit({
            type: 'checkpoint_restored',
            sandboxId,
            operatorId: sandboxId,
            beadId,
            data: { key: checkpoints[0] },
            timestamp: new Date(),
        });

        return true;
    }

    /** Clean up checkpoints after successful phase completion */
    async cleanCheckpoints(beadId: string): Promise<void> {
        const keys = await artifactStore.list(beadId);
        for (const key of keys.filter((k) => k.includes('checkpoint-'))) {
            await artifactStore.delete(key);
        }
    }
}

export const checkpointManager = new CheckpointManager();

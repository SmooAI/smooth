import { describe, expect, it } from 'vitest';

import type { AuditEntry } from './audit-log.js';

describe('AuditEntry types', () => {
    it('accepts a minimal entry', () => {
        const entry: AuditEntry = { actor: 'leader', action: 'tool_call' };
        expect(entry.actor).toBe('leader');
        expect(entry.action).toBe('tool_call');
    });

    it('accepts a full entry', () => {
        const entry: AuditEntry = {
            actor: 'operator-abc',
            action: 'tool_call',
            target: 'beads_context',
            beadId: 'bead-123',
            input: { beadId: 'bead-123' },
            output: { title: 'Fix bug' },
            durationMs: 245,
            metadata: { phase: 'execute' },
        };
        expect(entry.durationMs).toBe(245);
        expect(entry.target).toBe('beads_context');
    });

    it('accepts all action types', () => {
        const actions: AuditEntry['action'][] = [
            'tool_call',
            'tool_result',
            'prompt_sent',
            'prompt_received',
            'sandbox_created',
            'sandbox_destroyed',
            'phase_started',
            'phase_completed',
            'review_requested',
            'review_verdict',
            'bead_created',
            'bead_updated',
            'message_sent',
            'error',
        ];
        expect(actions).toHaveLength(14);
    });
});

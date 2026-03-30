import { describe, expect, it } from 'vitest';

import { PHASE_TIMEOUTS } from './worker-types.js';

describe('PHASE_TIMEOUTS', () => {
    it('has timeouts for all phases', () => {
        expect(PHASE_TIMEOUTS.assess).toBe(30 * 60);
        expect(PHASE_TIMEOUTS.plan).toBe(10 * 60);
        expect(PHASE_TIMEOUTS.orchestrate).toBe(15 * 60);
        expect(PHASE_TIMEOUTS.execute).toBe(60 * 60);
        expect(PHASE_TIMEOUTS.finalize).toBe(15 * 60);
    });

    it('all timeouts are positive', () => {
        for (const timeout of Object.values(PHASE_TIMEOUTS)) {
            expect(timeout).toBeGreaterThan(0);
        }
    });

    it('execute has the longest timeout', () => {
        const maxTimeout = Math.max(...Object.values(PHASE_TIMEOUTS));
        expect(PHASE_TIMEOUTS.execute).toBe(maxTimeout);
    });
});

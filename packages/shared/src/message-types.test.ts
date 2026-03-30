import { describe, expect, it } from 'vitest';

import { MESSAGE_PREFIXES } from './message-types.js';

describe('MESSAGE_PREFIXES', () => {
    it('has prefixes for all message directions', () => {
        expect(MESSAGE_PREFIXES['leader→worker']).toBe('[leader→worker]');
        expect(MESSAGE_PREFIXES['worker→leader']).toBe('[worker→leader]');
        expect(MESSAGE_PREFIXES['worker→worker']).toBe('[worker→worker]');
        expect(MESSAGE_PREFIXES['human→leader']).toBe('[human→leader]');
        expect(MESSAGE_PREFIXES['leader→human']).toBe('[leader→human]');
        expect(MESSAGE_PREFIXES.review).toBe('[review]');
        expect(MESSAGE_PREFIXES.progress).toBe('[progress]');
        expect(MESSAGE_PREFIXES.artifact).toBe('[artifact]');
    });

    it('all prefixes are bracket-wrapped', () => {
        for (const prefix of Object.values(MESSAGE_PREFIXES)) {
            expect(prefix.startsWith('[')).toBe(true);
            expect(prefix.endsWith(']')).toBe(true);
        }
    });
});

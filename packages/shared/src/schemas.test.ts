import { describe, expect, it } from 'vitest';

import { BeadFiltersSchema, ChatMessageSchema, CreateProjectSchema, ReviewVerdictSchema, SendMessageSchema, SetConfigSchema, WorkerPlanSchema } from './schemas.js';

describe('BeadFiltersSchema', () => {
    it('accepts empty filters', () => {
        expect(BeadFiltersSchema.parse({})).toEqual({});
    });

    it('accepts valid status', () => {
        expect(BeadFiltersSchema.parse({ status: 'open' })).toEqual({ status: 'open' });
    });

    it('rejects invalid status', () => {
        expect(() => BeadFiltersSchema.parse({ status: 'invalid' })).toThrow();
    });

    it('accepts valid phase', () => {
        expect(BeadFiltersSchema.parse({ phase: 'execute' })).toEqual({ phase: 'execute' });
    });
});

describe('SendMessageSchema', () => {
    it('accepts valid message', () => {
        const msg = { beadId: 'abc', content: 'hello', direction: 'human→leader' as const };
        expect(SendMessageSchema.parse(msg)).toEqual(msg);
    });

    it('rejects empty content', () => {
        expect(() => SendMessageSchema.parse({ beadId: 'abc', content: '', direction: 'human→leader' })).toThrow();
    });

    it('rejects invalid direction', () => {
        expect(() => SendMessageSchema.parse({ beadId: 'abc', content: 'hi', direction: 'invalid' })).toThrow();
    });
});

describe('ChatMessageSchema', () => {
    it('accepts message without attachments', () => {
        expect(ChatMessageSchema.parse({ content: 'hello' })).toEqual({ content: 'hello' });
    });

    it('accepts message with attachments', () => {
        const msg = {
            content: 'check this',
            attachments: [{ type: 'file' as const, reference: 'src/index.ts' }],
        };
        expect(ChatMessageSchema.parse(msg)).toEqual(msg);
    });
});

describe('WorkerPlanSchema', () => {
    it('accepts valid plan', () => {
        const plan = {
            steps: [{ description: 'step 1', tools: ['beads_context'], expectedOutput: 'context summary' }],
            needsSubWorkers: false,
            estimatedComplexity: 'low' as const,
        };
        expect(WorkerPlanSchema.parse(plan)).toEqual(plan);
    });
});

describe('ReviewVerdictSchema', () => {
    it('accepts approved verdict', () => {
        const verdict = { verdict: 'approved' as const, findings: [], suggestions: [], missing: [] };
        expect(ReviewVerdictSchema.parse(verdict)).toEqual(verdict);
    });

    it('accepts verdict with findings', () => {
        const verdict = {
            verdict: 'rework' as const,
            findings: [{ severity: 'high' as const, description: 'missing tests' }],
            suggestions: ['add unit tests'],
            missing: ['error handling'],
        };
        expect(ReviewVerdictSchema.parse(verdict)).toEqual(verdict);
    });
});

describe('CreateProjectSchema', () => {
    it('accepts valid project', () => {
        expect(CreateProjectSchema.parse({ name: 'test', description: 'desc' })).toEqual({ name: 'test', description: 'desc' });
    });

    it('rejects empty name', () => {
        expect(() => CreateProjectSchema.parse({ name: '', description: 'desc' })).toThrow();
    });
});

describe('SetConfigSchema', () => {
    it('accepts valid config', () => {
        expect(SetConfigSchema.parse({ key: 'jira.url', value: 'https://example.com' })).toEqual({ key: 'jira.url', value: 'https://example.com' });
    });
});

import assert from 'node:assert/strict';
import { describe, it } from 'node:test';

import { buildNotification } from './notify.js';

describe('buildNotification', () => {
    it('keeps a title + body', () => {
        assert.deepEqual(buildNotification({ title: 'Reminder', body: 'Standup in 5' }), { title: 'Reminder', body: 'Standup in 5' });
    });

    it('defaults a missing title to Big Smooth', () => {
        assert.deepEqual(buildNotification({ body: 'done' }), { title: 'Big Smooth', body: 'done' });
    });

    it('trims whitespace', () => {
        assert.deepEqual(buildNotification({ title: '  hi  ', body: '  there  ' }), { title: 'hi', body: 'there' });
    });

    it('returns null when there is nothing to show', () => {
        assert.equal(buildNotification({}), null);
        assert.equal(buildNotification({ title: '   ', body: '' }), null);
    });

    it('ignores non-string fields', () => {
        assert.equal(buildNotification({ title: 42, body: null }), null);
    });

    it('clamps over-long strings', () => {
        const out = buildNotification({ title: 'x'.repeat(500), body: 'y'.repeat(500) });
        assert.equal(out?.title.length, 120);
        assert.equal(out?.body.length, 240);
    });
});

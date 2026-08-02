import assert from 'node:assert/strict';
import { test } from 'node:test';

import { canSend } from './turn-guard.ts';

const base = { text: 'hello', attachments: 0, disabled: false, turnActive: false };

test('a normal draft sends', () => {
    assert.equal(canSend(base), true);
});

test('a turn in flight blocks send — this is the whole point (th-426791)', () => {
    assert.equal(canSend({ ...base, turnActive: true }), false);
    assert.equal(canSend({ ...base, text: '', attachments: 1, turnActive: true }), false);
});

test('a disconnected operator blocks send', () => {
    assert.equal(canSend({ ...base, disabled: true }), false);
});

test('an empty draft does not send, but an attachment alone does', () => {
    assert.equal(canSend({ ...base, text: '' }), false);
    assert.equal(canSend({ ...base, text: '   \n ' }), false);
    assert.equal(canSend({ ...base, text: '', attachments: 1 }), true);
});

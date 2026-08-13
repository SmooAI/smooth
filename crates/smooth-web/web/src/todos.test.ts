import assert from 'node:assert/strict';
import { test } from 'node:test';

import { normalizeTodos } from './todos.ts';

test('a well-formed todos list passes through', () => {
    assert.deepEqual(
        normalizeTodos([
            { text: 'a', status: 'completed' },
            { text: 'b', status: 'in_progress' },
            { text: 'c', status: 'pending' },
        ]),
        [
            { text: 'a', status: 'completed' },
            { text: 'b', status: 'in_progress' },
            { text: 'c', status: 'pending' },
        ],
    );
});

test('unknown/missing status clamps to pending', () => {
    assert.deepEqual(normalizeTodos([{ text: 'a' }, { text: 'b', status: 'weird' }]), [
        { text: 'a', status: 'pending' },
        { text: 'b', status: 'pending' },
    ]);
});

test('rows without a string text are dropped', () => {
    assert.deepEqual(normalizeTodos([{ status: 'completed' }, { text: 42 }, { text: 'keep' }]), [{ text: 'keep', status: 'pending' }]);
});

test('a non-array is empty, not a throw', () => {
    assert.deepEqual(normalizeTodos(undefined), []);
    assert.deepEqual(normalizeTodos(null), []);
    assert.deepEqual(normalizeTodos('nope'), []);
});

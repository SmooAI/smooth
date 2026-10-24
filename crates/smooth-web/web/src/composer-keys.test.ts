import assert from 'node:assert/strict';
import { test } from 'node:test';

import { enterAction, insertNewline } from './composer-keys.ts';

const key = (over: Partial<Parameters<typeof enterAction>[0]> = {}) => ({
    key: 'Enter',
    shiftKey: false,
    altKey: false,
    metaKey: false,
    ctrlKey: false,
    ...over,
});

test('plain Enter sends', () => {
    assert.equal(enterAction(key()), 'send');
});

test('Shift+Enter breaks the line', () => {
    assert.equal(enterAction(key({ shiftKey: true })), 'newline');
});

test('Alt/Option+Enter breaks the line instead of sending', () => {
    assert.equal(enterAction(key({ altKey: true })), 'newline');
});

test('Cmd or Ctrl+Enter steers', () => {
    assert.equal(enterAction(key({ metaKey: true })), 'steer');
    assert.equal(enterAction(key({ ctrlKey: true })), 'steer');
});

test('a newline modifier wins over a steer modifier', () => {
    assert.equal(enterAction(key({ ctrlKey: true, altKey: true })), 'newline');
});

test('other keys and IME Enter are not ours', () => {
    assert.equal(enterAction(key({ key: 'a' })), null);
    assert.equal(enterAction(key({ isComposing: true })), null);
});

test('insertNewline replaces the selection and advances the caret', () => {
    assert.deepEqual(insertNewline('hello', 5, 5), { text: 'hello\n', caret: 6 });
    assert.deepEqual(insertNewline('abXYcd', 2, 4), { text: 'ab\ncd', caret: 3 });
    assert.deepEqual(insertNewline('', 0, 0), { text: '\n', caret: 1 });
});

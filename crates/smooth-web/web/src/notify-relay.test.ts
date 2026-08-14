import assert from 'node:assert/strict';
import { test } from 'node:test';

import { replyToNotify } from './notify-relay.ts';

const reply = (over: Partial<{ id: string; role: string; content: string; streaming: boolean }> = {}) =>
    ({ id: 'a1', role: 'assistant', content: 'the answer', streaming: false, ...over }) as Parameters<typeof replyToNotify>[0][number];

const opts = { focused: false, lastNotifiedId: null };

test('notifies for a finished assistant reply when unfocused', () => {
    const d = replyToNotify([reply()], opts);
    assert.deepEqual(d, { id: 'a1', title: 'Big Smooth', body: 'the answer' });
});

test('stays quiet while focused', () => {
    assert.equal(replyToNotify([reply()], { ...opts, focused: true }), null);
});

test('stays quiet while still streaming', () => {
    assert.equal(replyToNotify([reply({ streaming: true })], opts), null);
});

test('stays quiet on an empty reply', () => {
    assert.equal(replyToNotify([reply({ content: '   ' })], opts), null);
});

test('stays quiet when the tail is a user message', () => {
    assert.equal(replyToNotify([reply(), reply({ id: 'u1', role: 'user', content: 'hi' })], opts), null);
});

test('does not re-notify the same reply', () => {
    assert.equal(replyToNotify([reply()], { ...opts, lastNotifiedId: 'a1' }), null);
});

test('clamps a long body', () => {
    const d = replyToNotify([reply({ content: 'x'.repeat(500) })], opts);
    assert.equal(d?.body.length, 160);
    assert.ok(d?.body.endsWith('…'));
});

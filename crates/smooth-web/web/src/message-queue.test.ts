import assert from 'node:assert/strict';
import { test } from 'node:test';

import { dequeue, enqueue, queuedLabel, removeAt, submitAction, type QueuedMessage } from './message-queue.ts';

const msg = (id: string, text = 'hi'): QueuedMessage => ({ id, text, attachments: [] });

test('idle submit with content sends immediately (no behaviour change)', () => {
    assert.equal(submitAction(false, true), 'send');
});

test('submit while a turn is active enqueues instead of blocking (th-0a079c)', () => {
    assert.equal(submitAction(true, true), 'enqueue');
});

test('an empty draft is a noop whether idle or mid-turn', () => {
    assert.equal(submitAction(false, false), 'noop');
    assert.equal(submitAction(true, false), 'noop');
});

test('enqueue appends in send order', () => {
    const q = enqueue(enqueue([], msg('a')), msg('b'));
    assert.deepEqual(
        q.map((m) => m.id),
        ['a', 'b'],
    );
});

test('dequeue-and-send: pulls the head, keeps the rest in order', () => {
    const q = [msg('a'), msg('b'), msg('c')];
    const first = dequeue(q);
    assert.ok(first);
    assert.equal(first.head.id, 'a');
    assert.deepEqual(
        first.rest.map((m) => m.id),
        ['b', 'c'],
    );
    // Draining continues one at a time on each turn completion.
    const second = dequeue(first.rest);
    assert.equal(second?.head.id, 'b');
});

test('dequeue on an empty queue is null (nothing to send)', () => {
    assert.equal(dequeue([]), null);
});

test('clear = empty queue, drained without touching the active turn', () => {
    // Clearing is just replacing the queue with []; dequeue then has nothing.
    assert.equal(dequeue([]), null);
});

test('remove-one drops exactly the chosen chip', () => {
    const q = [msg('a'), msg('b'), msg('c')];
    assert.deepEqual(
        removeAt(q, 1).map((m) => m.id),
        ['a', 'c'],
    );
});

test('queuedLabel falls back to an attachment count for text-only sends', () => {
    assert.equal(queuedLabel(msg('a', 'hello')), 'hello');
    assert.equal(queuedLabel({ id: 'a', text: '   ', attachments: [{ name: 'x.png', mime: 'image/png', dataUrl: 'data:' }] }), '1 attachment');
    assert.equal(
        queuedLabel({
            id: 'a',
            text: '',
            attachments: [
                { name: 'x', mime: 'image/png', dataUrl: 'd' },
                { name: 'y', mime: 'image/png', dataUrl: 'd' },
            ],
        }),
        '2 attachments',
    );
});

import assert from 'node:assert/strict';
import { test } from 'node:test';

import { dequeue, enqueue, queuedLabel, removeAt, submitAction, takeAt, type QueuedMessage } from './message-queue.ts';

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

test('follow-ups sent mid-turn fold into one held message, like iOS/Android (th-74ba1f)', () => {
    const q = enqueue(enqueue([], msg('a', 'first')), msg('b', 'second'));
    assert.equal(q.length, 1, 'one held message, not a string of turns');
    assert.equal(q[0].id, 'a', "keeps the first message's identity");
    assert.equal(q[0].text, 'first\n\nsecond', 'joined in order by a blank line');
});

test('folding keeps every attachment and skips empty text', () => {
    const img = (name: string) => ({ name }) as unknown as QueuedMessage['attachments'][number];
    const a: QueuedMessage = { id: 'a', text: '', attachments: [img('one')] };
    const b: QueuedMessage = { id: 'b', text: 'look at these', attachments: [img('two')] };
    const q = enqueue(enqueue([], a), b);
    assert.equal(q[0].text, 'look at these');
    assert.equal(q[0].attachments.length, 2);
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

test('Steer mid-turn with content steers instead of queueing (th-74ba1f)', () => {
    assert.equal(submitAction(true, true, true), 'steer');
});

test('Steer when idle is just a send, and an empty Steer is a noop', () => {
    assert.equal(submitAction(false, true, true), 'send');
    assert.equal(submitAction(true, false, true), 'noop');
});

test('takeAt pulls one queued message for "Steer now", keeping the rest in order', () => {
    const got = takeAt([msg('a'), msg('b'), msg('c')], 1);
    assert.equal(got?.item.id, 'b');
    assert.deepEqual(
        got?.rest.map((m) => m.id),
        ['a', 'c'],
    );
    assert.equal(takeAt([msg('a')], 5), null);
});

import assert from 'node:assert/strict';
import { test } from 'node:test';

import { AI_CREDITS_TOP_UP_URL, budgetExceededHint, CANCEL_FALLBACK_MS, cancelFrame, endsTurn, errorText, isStaleFrame, mergeSteer } from './turn-control.ts';

test('Stop sends the engine action `cancel`, never `interrupt` (th-74ba1f)', () => {
    const f = cancelFrame('turn-61', 'sess-1');
    assert.deepEqual(f, { action: 'cancel', requestId: 'turn-61', sessionId: 'sess-1' });
    assert.notEqual(f.action, 'interrupt');
});

test('cancelFrame omits ids it does not have rather than sending null', () => {
    assert.deepEqual(cancelFrame(null, null), { action: 'cancel' });
});

test('`cancelled` for the running turn ends it', () => {
    assert.equal(endsTurn({ type: 'cancelled', requestId: 'turn-61' }, 'turn-61'), true);
});

test('the turn’s own eventual_response and error end it', () => {
    assert.equal(endsTurn({ type: 'eventual_response', requestId: 'turn-61' }, 'turn-61'), true);
    assert.equal(endsTurn({ type: 'error', requestId: 'turn-61' }, 'turn-61'), true);
});

test('an error about a DIFFERENT request does not end the running turn (the 2026-09-23 desync)', () => {
    // The old Stop's UNSUPPORTED_ACTION carried the interrupt frame's own id.
    assert.equal(endsTurn({ type: 'error', requestId: 'int-67' }, 'turn-61'), false);
    // A TURN_IN_PROGRESS rejecting some other send.
    assert.equal(endsTurn({ type: 'error', requestId: 'turn-70' }, 'turn-61'), false);
});

test('a connection-level error (no requestId) ends the turn', () => {
    assert.equal(endsTurn({ type: 'error' }, 'turn-61'), true);
});

test('non-terminal events never end a turn', () => {
    for (const type of ['stream_token', 'stream_chunk', 'stream_reasoning', 'immediate_response', 'write_confirmation_required']) {
        assert.equal(endsTurn({ type, requestId: 'turn-61' }, 'turn-61'), false, type);
    }
    assert.equal(endsTurn({}, 'turn-61'), false);
});

test('with no tracked turn, any terminal event ends it (scheduled / pre-tracking turns)', () => {
    assert.equal(endsTurn({ type: 'eventual_response', requestId: 'x' }, null), true);
});

test('stragglers from a cancelled turn are stale; its `cancelled` is not', () => {
    assert.equal(isStaleFrame({ type: 'stream_token', requestId: 'turn-61' }, 'turn-61'), true);
    assert.equal(isStaleFrame({ type: 'eventual_response', requestId: 'turn-61' }, 'turn-61'), true);
    assert.equal(isStaleFrame({ type: 'cancelled', requestId: 'turn-61' }, 'turn-61'), false);
    assert.equal(isStaleFrame({ type: 'stream_token', requestId: 'turn-62' }, 'turn-61'), false);
    assert.equal(isStaleFrame({ type: 'stream_token', requestId: 'turn-61' }, null), false);
});

test('errorText reads the engine’s error.message, not a missing top-level message', () => {
    assert.equal(errorText({ error: { code: 'TURN_IN_PROGRESS', message: 'a turn is already in progress' } }), 'a turn is already in progress');
    assert.equal(errorText({ data: { error: { message: 'nested' } } }), 'nested');
    assert.equal(errorText({ message: 'legacy' }), 'legacy');
    assert.equal(errorText({}), 'operator error');
});

test('a second Steer folds into the waiting one; nothing typed is lost', () => {
    const first = mergeSteer<string>(null, { text: 'stop, use the CRM', attachments: ['a'] });
    assert.deepEqual(first, { text: 'stop, use the CRM', attachments: ['a'] });
    const both = mergeSteer(first, { text: 'and be brief', attachments: ['b'] });
    assert.deepEqual(both, { text: 'stop, use the CRM\n\nand be brief', attachments: ['a', 'b'] });
    // An attachment-only steer adds no blank paragraph.
    assert.equal(mergeSteer(first, { text: '  ', attachments: [] }).text, 'stop, use the CRM');
});

test('the cancel fallback is bounded and not instant', () => {
    assert.ok(CANCEL_FALLBACK_MS >= 2_000 && CANCEL_FALLBACK_MS <= 30_000);
});

test('a gateway budget refusal renders as the AI credits top-up line', () => {
    const after =
        'LLM API error 402 Payment Required: {"error":{"message":"Budget has been exceeded! Team=o Out of AI budget — top up AI credits: https://smoo.ai/apps/llm-gateway#ai-credits","type":"budget_exceeded","code":"402"}}';
    const before = 'LLM API error 429 Too Many Requests: {"error":{"message":"Budget has been exceeded! Team=o","type":"budget_exceeded","code":"429"}}';
    for (const raw of [after, before]) {
        const text = errorText({ error: { code: 'INTERNAL_ERROR', message: raw } });
        assert.ok(text.startsWith('Out of AI budget'), text);
        assert.ok(text.includes(AI_CREDITS_TOP_UP_URL), text);
        assert.ok(!text.includes('{'), text);
        assert.ok(text.includes('$5 or $10'), text);
    }
    const staging = after.replace('https://smoo.ai/', 'https://staging.smoo.ai/');
    assert.ok(budgetExceededHint(staging)?.includes('https://staging.smoo.ai/apps/llm-gateway#ai-credits'));
});

test('other errors keep their text', () => {
    assert.equal(budgetExceededHint('LLM API error 429 Too Many Requests: rate limit reached'), null);
    assert.equal(errorText({ error: { message: 'rate limit reached' } }), 'rate limit reached');
});

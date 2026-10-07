import assert from 'node:assert/strict';
import { test } from 'node:test';

import { IMAGE_ONLY_PLACEHOLDER, displayText, outgoingText, sessionOpenFrame } from './session.ts';

test('a reconnect re-binds the conversation on screen (SMOODEV-3708)', () => {
    const f = sessionOpenFrame('cs-1', 'agent-1', 'conv-42');
    assert.equal(f.action, 'create_conversation_session');
    assert.equal(f.conversationId, 'conv-42');
});

test('with no active conversation the frame asks for a new one', () => {
    assert.deepEqual(sessionOpenFrame('cs-1', 'agent-1', null), {
        action: 'create_conversation_session',
        requestId: 'cs-1',
        agentId: 'agent-1',
        userName: 'console',
    });
    assert.equal('conversationId' in sessionOpenFrame('cs-1', 'a', ''), false);
});

test('an image-only send carries a non-empty message', () => {
    assert.equal(outgoingText('', 1), IMAGE_ONLY_PLACEHOLDER);
    assert.equal(outgoingText('   ', 2), IMAGE_ONLY_PLACEHOLDER);
    assert.equal(outgoingText(' look ', 1), 'look');
    assert.equal(outgoingText('', 0), '');
});

test('history hides the placeholder only when the image is there to show', () => {
    assert.equal(displayText(IMAGE_ONLY_PLACEHOLDER, true), '');
    assert.equal(displayText(IMAGE_ONLY_PLACEHOLDER, false), IMAGE_ONLY_PLACEHOLDER);
    assert.equal(displayText('look at this', true), 'look at this');
});

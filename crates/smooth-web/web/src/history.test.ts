import assert from 'node:assert/strict';
import { test } from 'node:test';

import { historyImages, historyTurnError, type HistoryMessage } from './history.ts';

// th-1fca98: a history user turn now carries persisted `image` content items so
// this client re-renders images another client attached. historyImages turns
// those into renderable attachments (App.tsx renders mime.startsWith('image/')).

test('historyImages extracts image items with the mime from a data: URL', () => {
    const m: HistoryMessage = {
        direction: 'inbound',
        content: {
            items: [
                { type: 'text', text: 'look at this' },
                { type: 'image', url: 'data:image/png;base64,AAAA' },
            ],
        },
    };
    const imgs = historyImages(m);
    assert.equal(imgs.length, 1);
    assert.equal(imgs[0].mime, 'image/png');
    assert.equal(imgs[0].dataUrl, 'data:image/png;base64,AAAA');
    assert.ok(imgs[0].mime.startsWith('image/')); // the App.tsx render gate
});

test('historyImages defaults https image URLs to image/* so they still render', () => {
    const m: HistoryMessage = {
        content: { items: [{ type: 'image', url: 'https://cdn/x.png' }] },
    };
    const imgs = historyImages(m);
    assert.equal(imgs.length, 1);
    assert.equal(imgs[0].mime, 'image/*');
    assert.ok(imgs[0].mime.startsWith('image/'));
});

test('historyImages is empty for text-only turns and old daemons (no image items)', () => {
    assert.deepEqual(historyImages({ content: { items: [{ type: 'text', text: 'hi' }] } }), []);
    assert.deepEqual(historyImages({ content: 'plain string' }), []);
    assert.deepEqual(historyImages({ text: 'alias' }), []);
});

test('historyTurnError reads a persisted failed-turn record (SMOODEV-3705)', () => {
    const m: HistoryMessage = {
        direction: 'outbound',
        content: { items: [{ type: 'text', text: 'This reply failed before it finished (boom).' }] },
        metadataJson: { turnError: { code: 'LLM_ERROR', requestId: 'turn-3' } },
    };
    assert.deepEqual(historyTurnError(m), { code: 'LLM_ERROR', requestId: 'turn-3' });
});

test('historyTurnError is null for ordinary messages and tolerates a missing code', () => {
    assert.equal(historyTurnError({ direction: 'outbound', content: 'hi' }), null);
    assert.equal(historyTurnError({ direction: 'outbound', metadataJson: null }), null);
    assert.equal(historyTurnError({ direction: 'outbound', metadataJson: {} }), null);
    assert.deepEqual(historyTurnError({ direction: 'outbound', metadataJson: { turnError: {} } }), { code: 'TURN_ERROR', requestId: undefined });
});

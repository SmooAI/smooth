import assert from 'node:assert/strict';
import { describe, it } from 'node:test';

import { isNewChatChord } from './newchat.js';

/** A minimal Electron-`Input`-shaped object for the chord matcher. */
function input(over: Partial<{ type: string; key: string; control: boolean; meta: boolean; alt: boolean; shift: boolean }>) {
    return { type: 'keyDown', key: 'n', control: false, meta: false, alt: false, shift: false, ...over };
}

describe('isNewChatChord', () => {
    it('matches Cmd+N (macOS)', () => {
        assert.equal(isNewChatChord(input({ meta: true })), true);
    });

    it('matches Ctrl+N (Windows/Linux)', () => {
        assert.equal(isNewChatChord(input({ control: true })), true);
    });

    it('is case-insensitive on the key', () => {
        assert.equal(isNewChatChord(input({ meta: true, key: 'N' })), true);
    });

    it('ignores a bare N with no modifier', () => {
        assert.equal(isNewChatChord(input({})), false);
    });

    it('ignores other letters with the modifier', () => {
        assert.equal(isNewChatChord(input({ meta: true, key: 't' })), false);
    });

    it('does not fire on key-up (only key-down)', () => {
        assert.equal(isNewChatChord(input({ meta: true, type: 'keyUp' })), false);
    });

    it('excludes Cmd+Shift+N and Cmd+Alt+N (reserved chords)', () => {
        assert.equal(isNewChatChord(input({ meta: true, shift: true })), false);
        assert.equal(isNewChatChord(input({ meta: true, alt: true })), false);
    });
});

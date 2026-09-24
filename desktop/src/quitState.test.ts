import assert from 'node:assert/strict';
import { beforeEach, describe, it } from 'node:test';

import { isQuitting, markQuitting, resetQuittingForTests, shouldHideOnClose } from './quitState.js';

describe('quit state (th-6b5d5c)', () => {
    beforeEach(() => resetQuittingForTests());

    it('a plain window close hides to the tray', () => {
        assert.equal(isQuitting(), false);
        assert.equal(shouldHideOnClose(), true);
    });

    it('once an exit starts, the window close is let through', () => {
        // The update path: the updater marks the quit BEFORE quitAndInstall()
        // closes the windows, so the close handler must not cancel them.
        markQuitting();
        assert.equal(isQuitting(), true);
        assert.equal(shouldHideOnClose(), false, 'cancelling this close is what kept the old app alive and blocked the install');
    });

    it('marking is idempotent', () => {
        markQuitting();
        markQuitting();
        assert.equal(shouldHideOnClose(), false);
    });
});

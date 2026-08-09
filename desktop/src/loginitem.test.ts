import assert from 'node:assert/strict';
import { test } from 'node:test';

import { firstRunLoginItem, trayModeLabel } from './loginitem.js';

test('firstRunLoginItem enables open-at-login exactly once', () => {
    // Never configured → enable it (and the caller will mark it configured).
    assert.deepEqual(firstRunLoginItem(false), { setOpenAtLogin: true });
    // Already configured → step aside, the user's toggle owns it now.
    assert.equal(firstRunLoginItem(true), null);
});

test('trayModeLabel names the remote host but says the local daemon still runs', () => {
    assert.equal(trayModeLabel(null), 'Running on This Mac');
    assert.equal(trayModeLabel('smoo-hub'), "Viewing smoo-hub · This Mac's daemon still running");
});

import assert from 'node:assert/strict';
import { mkdtempSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';

import { baseUrl, isRemote, remoteUrl, resolveAddr, resolveDaemonBin } from './daemon.js';

test('resolveAddr defaults, overrides, and ignores blanks', () => {
    assert.equal(resolveAddr({}), '127.0.0.1:8787');
    assert.equal(resolveAddr({ SMOOTH_ADDR: '  ' }), '127.0.0.1:8787');
    assert.equal(resolveAddr({ SMOOTH_ADDR: '127.0.0.1:8788' }), '127.0.0.1:8788');
    assert.equal(baseUrl({ SMOOTH_ADDR: '127.0.0.1:8788' }), 'http://127.0.0.1:8788');
});

test('remote URL takes over the target and trims a trailing slash', () => {
    const env = { SMOOTH_REMOTE_URL: 'https://smoo-hub.tailnet.ts.net:8443/' };
    assert.equal(remoteUrl(env), 'https://smoo-hub.tailnet.ts.net:8443/');
    assert.equal(isRemote(env), true);
    // Remote wins over a local SMOOTH_ADDR, and the trailing slash is dropped.
    assert.equal(baseUrl({ ...env, SMOOTH_ADDR: '127.0.0.1:8788' }), 'https://smoo-hub.tailnet.ts.net:8443');
});

test('no/blank remote URL means local', () => {
    assert.equal(isRemote({}), false);
    assert.equal(isRemote({ SMOOTH_REMOTE_URL: '  ' }), false);
    assert.equal(baseUrl({ SMOOTH_REMOTE_URL: '' }), 'http://127.0.0.1:8787');
});

test('resolveDaemonBin takes the first candidate that exists', () => {
    const dir = mkdtempSync(join(tmpdir(), 'smooth-desktop-'));
    const real = join(dir, 'smooth-daemon');
    writeFileSync(real, '');
    assert.equal(resolveDaemonBin([join(dir, 'missing'), '', real]), real);
    assert.equal(resolveDaemonBin([join(dir, 'missing')]), undefined);
});

import assert from 'node:assert/strict';
import { mkdtempSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';

import { baseUrl, isRemote, remoteUrl, resolveAddr, resolveDaemonBin, tccHelperApp, tccOpenArgs } from './daemon.js';

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

test('tccHelperApp resolves the sibling helper under Contents/Helpers on macOS', () => {
    // resourcesPath is <app>/Contents/Resources; the helper is Contents/Helpers/...
    assert.equal(tccHelperApp('/Applications/Big Smooth.app/Contents/Resources', 'darwin'), '/Applications/Big Smooth.app/Contents/Helpers/BigSmoothTCC.app');
    // Off macOS, or with no resourcesPath (unpackaged), there is no helper.
    assert.equal(tccHelperApp('/whatever/Contents/Resources', 'win32'), undefined);
    assert.equal(tccHelperApp(undefined, 'darwin'), undefined);
});

test('tccOpenArgs builds a fresh-instance `open` invocation forwarding the tcc verb', () => {
    assert.deepEqual(tccOpenArgs('/x/BigSmoothTCC.app', 'calendar'), ['-n', '/x/BigSmoothTCC.app', '--args', 'tcc', 'calendar']);
    assert.deepEqual(tccOpenArgs('/x/BigSmoothTCC.app', 'reminders'), ['-n', '/x/BigSmoothTCC.app', '--args', 'tcc', 'reminders']);
});

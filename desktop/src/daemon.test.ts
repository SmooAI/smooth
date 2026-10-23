import assert from 'node:assert/strict';
import { mkdtempSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';

import { baseUrl, daemonThEnv, desktopLogPath, isRemote, remoteUrl, resolveAddr, newestTh, resolveDaemonBin, tccHelperApp, tccOpenArgs } from './daemon.js';

// A path that does not exist → resolveAddr falls through to its default. Pinning
// the addrFile keeps these deterministic regardless of any real ~/.smooth/daemon.addr.
const NO_ADDR_FILE = join(tmpdir(), 'definitely-absent-daemon.addr');

test('resolveAddr defaults, overrides, and ignores blanks', () => {
    assert.equal(resolveAddr({}, NO_ADDR_FILE), '127.0.0.1:8787');
    assert.equal(resolveAddr({ SMOOTH_ADDR: '  ' }, NO_ADDR_FILE), '127.0.0.1:8787');
    assert.equal(resolveAddr({ SMOOTH_ADDR: '127.0.0.1:8788' }, NO_ADDR_FILE), '127.0.0.1:8788');
    assert.equal(baseUrl({ SMOOTH_ADDR: '127.0.0.1:8788' }), 'http://127.0.0.1:8788');
});

test('resolveAddr reads the daemon-advertised addr when SMOOTH_ADDR is unset', () => {
    const dir = mkdtempSync(join(tmpdir(), 'bs-addr-'));
    const f = join(dir, 'daemon.addr');
    // Daemon advertised :8788 (e.g. smoo-hub, where :8787 is the dashboard).
    writeFileSync(f, '127.0.0.1:8788\n');
    assert.equal(resolveAddr({}, f), '127.0.0.1:8788');
    // An explicit env override still wins over the file.
    assert.equal(resolveAddr({ SMOOTH_ADDR: '127.0.0.1:9000' }, f), '127.0.0.1:9000');
    // Garbage in the file → clean fall-back to the default, never a bad URL.
    writeFileSync(f, 'not-a-host-port');
    assert.equal(resolveAddr({}, f), '127.0.0.1:8787');
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

test('desktopLogPath lives under ~/.smooth so a Finder-launched daemon logs somewhere findable', () => {
    assert.match(desktopLogPath(), /[/\\]\.smooth[/\\]desktop\.log$/);
});

// SMOODEV-4114: the daemon's `th` is the newest of brew's and the bundled one.
const versions: Record<string, string | undefined> = { '/brew/th': '0.81.1', '/app/th': '0.84.1', '/new-brew/th': '0.90.0', '/broken/th': undefined };
const versionOf = (bin: string) => versions[bin];

test('daemonThEnv never downgrades to an older Homebrew th', () => {
    const env = daemonThEnv({}, { brew: () => '/brew/th', bundled: () => '/app/th', versionOf });
    assert.deepEqual(env, { SMOOTH_TH_BIN: '/app/th' });
});

test('daemonThEnv keeps brew when it is newer or equal', () => {
    assert.deepEqual(daemonThEnv({}, { brew: () => '/new-brew/th', bundled: () => '/app/th', versionOf }), { SMOOTH_TH_BIN: '/new-brew/th' });
    const tie = (bin: string) => (bin ? '0.84.1' : undefined);
    assert.deepEqual(daemonThEnv({}, { brew: () => '/brew/th', bundled: () => '/app/th', versionOf: tie }), { SMOOTH_TH_BIN: '/brew/th' });
});

test('daemonThEnv: an explicit SMOOTH_TH_BIN wins, and a lone th is used as is', () => {
    assert.deepEqual(daemonThEnv({ SMOOTH_TH_BIN: '/pinned/th' }, { brew: () => '/brew/th', bundled: () => '/app/th', versionOf }), {});
    assert.deepEqual(daemonThEnv({}, { brew: () => undefined, bundled: () => '/app/th', versionOf }), { SMOOTH_TH_BIN: '/app/th' });
    assert.deepEqual(daemonThEnv({}, { brew: () => '/brew/th', bundled: () => undefined, versionOf }), { SMOOTH_TH_BIN: '/brew/th' });
    assert.deepEqual(daemonThEnv({}, { brew: () => undefined, bundled: () => undefined, versionOf }), {});
});

test('newestTh prefers the readable version, and the preferred one when neither reads', () => {
    assert.equal(newestTh('/broken/th', '/app/th', versionOf), '/app/th');
    assert.equal(newestTh('/brew/th', '/broken/th', versionOf), '/brew/th');
    assert.equal(newestTh('/broken/th', '/broken/th', versionOf), '/broken/th');
    assert.equal(newestTh('/broken/th', '/other-broken/th', versionOf), '/broken/th');
});

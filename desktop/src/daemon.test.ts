import assert from 'node:assert/strict';
import { mkdtempSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';

import { baseUrl, resolveAddr, resolveDaemonBin } from './daemon.js';

test('resolveAddr defaults, overrides, and ignores blanks', () => {
    assert.equal(resolveAddr({}), '127.0.0.1:8787');
    assert.equal(resolveAddr({ SMOOTH_ADDR: '  ' }), '127.0.0.1:8787');
    assert.equal(resolveAddr({ SMOOTH_ADDR: '127.0.0.1:8788' }), '127.0.0.1:8788');
    assert.equal(baseUrl({ SMOOTH_ADDR: '127.0.0.1:8788' }), 'http://127.0.0.1:8788');
});

test('resolveDaemonBin takes the first candidate that exists', () => {
    const dir = mkdtempSync(join(tmpdir(), 'smooth-desktop-'));
    const real = join(dir, 'smooth-daemon');
    writeFileSync(real, '');
    assert.equal(resolveDaemonBin([join(dir, 'missing'), '', real]), real);
    assert.equal(resolveDaemonBin([join(dir, 'missing')]), undefined);
});

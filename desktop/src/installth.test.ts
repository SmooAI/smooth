import assert from 'node:assert/strict';
import { existsSync, mkdirSync, mkdtempSync, readlinkSync, symlinkSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';

import { linkThOnPath } from './installth.js';

function tmp(): string {
    return mkdtempSync(join(tmpdir(), 'installth-'));
}

test('creates a symlink when the target is absent', () => {
    const dir = tmp();
    const bundled = join(dir, 'bundle-th');
    writeFileSync(bundled, '#!/bin/sh\n');
    const target = join(dir, 'bin', 'th');
    const res = linkThOnPath(bundled, [target]);
    assert.equal(res.action, 'created');
    assert.equal(readlinkSync(target), bundled);
});

test('leaves a regular file untouched (brew/curl th)', () => {
    const dir = tmp();
    const bundled = join(dir, 'bundle-th');
    writeFileSync(bundled, '#!/bin/sh\n');
    mkdirSync(join(dir, 'bin'));
    const target = join(dir, 'bin', 'th');
    writeFileSync(target, 'REAL BREW TH');
    const res = linkThOnPath(bundled, [target]);
    assert.equal(res.action, 'skipped-regular-file');
    assert.equal(existsSync(target), true);
    // Still the original file, not a symlink.
    assert.throws(() => readlinkSync(target));
});

test('repoints a stale symlink to the bundled binary', () => {
    const dir = tmp();
    const bundled = join(dir, 'bundle-th');
    const stale = join(dir, 'old-th');
    writeFileSync(bundled, '#!/bin/sh\n');
    writeFileSync(stale, '#!/bin/sh\n');
    mkdirSync(join(dir, 'bin'));
    const target = join(dir, 'bin', 'th');
    symlinkSync(stale, target);
    const res = linkThOnPath(bundled, [target]);
    assert.equal(res.action, 'repointed');
    assert.equal(readlinkSync(target), bundled);
});

test('no-op when the symlink already points at the bundled binary', () => {
    const dir = tmp();
    const bundled = join(dir, 'bundle-th');
    writeFileSync(bundled, '#!/bin/sh\n');
    mkdirSync(join(dir, 'bin'));
    const target = join(dir, 'bin', 'th');
    symlinkSync(bundled, target);
    const res = linkThOnPath(bundled, [target]);
    assert.equal(res.action, 'current');
});

test('falls back to the next target when the first is unwritable', () => {
    const dir = tmp();
    const bundled = join(dir, 'bundle-th');
    writeFileSync(bundled, '#!/bin/sh\n');
    // First target lives under a path component that is a FILE, so mkdir fails.
    const blocker = join(dir, 'blocker');
    writeFileSync(blocker, 'x');
    const bad = join(blocker, 'bin', 'th');
    const good = join(dir, 'good', 'th');
    const res = linkThOnPath(bundled, [bad, good]);
    assert.equal(res.action, 'created');
    assert.equal(res.path, good);
});

test('unsupported when there is no bundled binary', () => {
    const res = linkThOnPath(undefined, ['/nope/th']);
    assert.equal(res.action, 'unsupported');
});

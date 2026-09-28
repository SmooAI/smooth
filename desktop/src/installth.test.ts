import assert from 'node:assert/strict';
import { chmodSync, existsSync, mkdirSync, mkdtempSync, readlinkSync, symlinkSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { test } from 'node:test';

import { compareVersions, linkThOnPath, ownedLinkDest, parseVersion, thVersion } from './installth.js';

function tmp(): string {
    return mkdtempSync(join(tmpdir(), 'installth-'));
}

/** An executable fake `th` that answers `--version` like the real one. */
function fakeTh(path: string, version: string): string {
    mkdirSync(dirname(path), { recursive: true });
    writeFileSync(path, `#!/bin/sh\necho "th ${version} (abc1234)"\n`);
    chmodSync(path, 0o755);
    return path;
}

/** The bundled th of an app at `dir/<name>.app`. */
function bundleTh(dir: string, name: string, version: string): string {
    return fakeTh(join(dir, `${name}.app`, 'Contents', 'Resources', 'th'), version);
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

test('repoints a link into an older app bundle', () => {
    const dir = tmp();
    const bundled = bundleTh(dir, 'Big Smooth', '0.54.0');
    const stale = bundleTh(join(dir, 'old'), 'Big Smooth', '0.50.2');
    mkdirSync(join(dir, 'bin'));
    const target = join(dir, 'bin', 'th');
    symlinkSync(stale, target);
    const res = linkThOnPath(bundled, [target]);
    assert.equal(res.action, 'repointed');
    assert.equal(readlinkSync(target), bundled);
});

// The 2026-09-28 incident: `~/.local/bin/th → /opt/homebrew/bin/th` (0.58.0),
// bundled 0.54.0. Repointing it took away `th ci-queue` machine-wide.
test('never repoints a link it did not make (brew th), even to a newer bundle', () => {
    const dir = tmp();
    const brew = fakeTh(join(dir, 'opt', 'homebrew', 'bin', 'th'), '0.58.0');
    mkdirSync(join(dir, 'bin'));
    const target = join(dir, 'bin', 'th');
    symlinkSync(brew, target);
    for (const version of ['0.54.0', '0.99.0']) {
        const bundled = bundleTh(join(dir, version), 'Big Smooth', version);
        const res = linkThOnPath(bundled, [target]);
        assert.equal(res.action, 'skipped-foreign-link', `bundled ${version}`);
        assert.equal(readlinkSync(target), brew);
    }
});

test('never downgrades: a link into another bundle running a newer th stays', () => {
    const dir = tmp();
    const bundled = bundleTh(dir, 'Big Smooth', '0.54.0');
    const newer = bundleTh(join(dir, 'beta'), 'Big Smooth Beta', '0.58.0');
    mkdirSync(join(dir, 'bin'));
    const target = join(dir, 'bin', 'th');
    symlinkSync(newer, target);
    const res = linkThOnPath(bundled, [target]);
    assert.equal(res.action, 'skipped-newer');
    assert.match(res.note ?? '', /0\.58\.0.*0\.54\.0/);
    assert.equal(readlinkSync(target), newer);
});

test('keeps an owned link when the bundled th reports no version', () => {
    const dir = tmp();
    const bundled = join(dir, 'Big Smooth.app', 'Contents', 'Resources', 'th');
    mkdirSync(dirname(bundled), { recursive: true });
    writeFileSync(bundled, '#!/bin/sh\nexit 1\n');
    chmodSync(bundled, 0o755);
    const older = bundleTh(join(dir, 'old'), 'Big Smooth', '0.50.0');
    mkdirSync(join(dir, 'bin'));
    const target = join(dir, 'bin', 'th');
    symlinkSync(older, target);
    const res = linkThOnPath(bundled, [target]);
    assert.equal(res.action, 'skipped-unknown-version');
    assert.equal(readlinkSync(target), older);
});

test('repoints a dangling link (nothing runs there anyway)', () => {
    const dir = tmp();
    const bundled = bundleTh(dir, 'Big Smooth', '0.54.0');
    mkdirSync(join(dir, 'bin'));
    const target = join(dir, 'bin', 'th');
    symlinkSync(join(dir, 'gone', 'th'), target);
    const res = linkThOnPath(bundled, [target]);
    assert.equal(res.action, 'repointed');
    assert.equal(readlinkSync(target), bundled);
});

test('the version guard reads the versions it is handed', () => {
    const dir = tmp();
    const bundled = bundleTh(dir, 'Big Smooth', '0.54.0');
    const other = bundleTh(join(dir, 'x'), 'Big Smooth', '0.54.0');
    mkdirSync(join(dir, 'bin'));
    const target = join(dir, 'bin', 'th');
    symlinkSync(other, target);
    const seen: string[] = [];
    const res = linkThOnPath(bundled, [target], (bin) => {
        seen.push(bin);
        return bin === bundled ? '1.0.0' : '1.0.1';
    });
    assert.equal(res.action, 'skipped-newer');
    assert.deepEqual(seen.sort(), [bundled, target].sort());
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

test('never links a th to itself', () => {
    const dir = tmp();
    const brew = fakeTh(join(dir, 'brew', 'th'), '0.58.0');
    mkdirSync(join(dir, 'bin'));
    const target = join(dir, 'bin', 'th');
    symlinkSync(brew, target);
    // What an unpackaged run's PATH fallback would have handed in.
    const res = linkThOnPath(target, [target]);
    assert.equal(res.action, 'current');
    assert.equal(readlinkSync(target), brew);
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

test('compareVersions orders major.minor.patch numerically', () => {
    assert.ok(compareVersions('0.58.0', '0.54.0') > 0);
    assert.ok(compareVersions('0.54.0', '0.58.0') < 0);
    assert.ok(compareVersions('0.10.0', '0.9.9') > 0, 'numeric, not lexical');
    assert.ok(compareVersions('1.0.0', '0.99.99') > 0);
    assert.ok(compareVersions('0.58.1', '0.58.0') > 0);
    assert.equal(compareVersions('0.58.0', '0.58.0'), 0);
});

test('parseVersion reads th --version output', () => {
    assert.equal(parseVersion('th 0.58.0 (3719cc9)\n'), '0.58.0');
    assert.equal(parseVersion('th 0.54.0'), '0.54.0');
    assert.equal(parseVersion('error: no'), undefined);
});

test('thVersion runs the binary, and is undefined when it cannot', () => {
    const dir = tmp();
    assert.equal(thVersion(fakeTh(join(dir, 'th'), '0.58.0')), '0.58.0');
    assert.equal(thVersion(join(dir, 'missing')), undefined);
});

test('ownedLinkDest: only app-bundle resources are ours', () => {
    assert.ok(ownedLinkDest('/Applications/Big Smooth.app/Contents/Resources/th'));
    assert.ok(ownedLinkDest('/Applications/Big Smooth.app/Contents/Resources/bin/th'));
    assert.ok(!ownedLinkDest('/opt/homebrew/bin/th'));
    assert.ok(!ownedLinkDest('/Users/me/.cargo/bin/th'));
});

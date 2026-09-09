import assert from 'node:assert/strict';
import { existsSync, mkdtempSync, readFileSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { describe, it } from 'node:test';

import { daemonLogDir, RotatingLog, rotateFiles } from './daemonlog.js';

describe('daemonLogDir', () => {
    it('is the Console-visible Library/Logs folder on macOS and ~/.smooth/logs elsewhere', () => {
        assert.equal(daemonLogDir({}, 'darwin', '/Users/b'), '/Users/b/Library/Logs/Big Smooth');
        assert.equal(daemonLogDir({}, 'linux', '/home/b'), '/home/b/.smooth/logs');
        assert.equal(daemonLogDir({}, 'win32', 'C:\\Users\\b'), join('C:\\Users\\b', '.smooth', 'logs'));
    });

    it('honours SMOOTH_DESKTOP_LOG_DIR (tests, second instances) and ignores a blank one', () => {
        assert.equal(daemonLogDir({ SMOOTH_DESKTOP_LOG_DIR: '/tmp/x' }, 'darwin', '/Users/b'), '/tmp/x');
        assert.equal(daemonLogDir({ SMOOTH_DESKTOP_LOG_DIR: '  ' }, 'darwin', '/Users/b'), '/Users/b/Library/Logs/Big Smooth');
    });
});

describe('RotatingLog', () => {
    it('creates the directory, timestamps lines, and appends raw chunks verbatim', () => {
        const dir = mkdtempSync(join(tmpdir(), 'bs-log-'));
        const path = join(dir, 'nested', 'daemon.log');
        const log = new RotatingLog(path, { now: () => new Date('2026-09-09T12:00:00.000Z') });
        log.line('spawned pid 1');
        log.raw('stderr says hi\n');
        log.raw(Buffer.from('bytes too\n'));
        log.close();
        assert.equal(readFileSync(path, 'utf8'), '2026-09-09T12:00:00.000Z spawned pid 1\nstderr says hi\nbytes too\n');
    });

    it('rotates when a write would push past maxBytes and keeps N generations', () => {
        const dir = mkdtempSync(join(tmpdir(), 'bs-log-'));
        const path = join(dir, 'daemon.log');
        const log = new RotatingLog(path, { maxBytes: 20, keep: 2 });
        log.raw('aaaaaaaaaa\n'); // 11 bytes
        log.raw('bbbbbbbbbb\n'); // 22 > 20 → rotate first
        assert.equal(readFileSync(`${path}.1`, 'utf8'), 'aaaaaaaaaa\n');
        assert.equal(readFileSync(path, 'utf8'), 'bbbbbbbbbb\n');
        log.raw('cccccccccc\n');
        log.raw('dddddddddd\n');
        log.close();
        assert.equal(readFileSync(path, 'utf8'), 'dddddddddd\n');
        assert.equal(readFileSync(`${path}.1`, 'utf8'), 'cccccccccc\n');
        assert.equal(readFileSync(`${path}.2`, 'utf8'), 'bbbbbbbbbb\n');
        assert.equal(existsSync(`${path}.3`), false, 'the oldest generation is dropped');
    });

    it('a single oversized chunk still lands (never rotates an empty file forever)', () => {
        const dir = mkdtempSync(join(tmpdir(), 'bs-log-'));
        const path = join(dir, 'daemon.log');
        const log = new RotatingLog(path, { maxBytes: 4, keep: 1 });
        log.raw('0123456789\n');
        log.close();
        assert.equal(readFileSync(path, 'utf8'), '0123456789\n');
        assert.equal(existsSync(`${path}.1`), false);
    });

    it('picks up the existing size so an old file rotates on the first write past the cap', () => {
        const dir = mkdtempSync(join(tmpdir(), 'bs-log-'));
        const path = join(dir, 'daemon.log');
        writeFileSync(path, 'x'.repeat(30));
        const log = new RotatingLog(path, { maxBytes: 32, keep: 1 });
        log.raw('yyy\n');
        log.close();
        assert.equal(readFileSync(`${path}.1`, 'utf8'), 'x'.repeat(30));
        assert.equal(readFileSync(path, 'utf8'), 'yyy\n');
    });

    it('never throws when the path is unwritable', () => {
        const log = new RotatingLog('/dev/null/not-a-dir/daemon.log');
        assert.doesNotThrow(() => log.line('hello'));
        assert.doesNotThrow(() => log.close());
    });
});

describe('rotateFiles', () => {
    it('shifts generations and truncates with keep=0', () => {
        const dir = mkdtempSync(join(tmpdir(), 'bs-rot-'));
        const path = join(dir, 'f.log');
        writeFileSync(path, 'live');
        writeFileSync(`${path}.1`, 'one');
        writeFileSync(`${path}.2`, 'two');
        rotateFiles(path, 2);
        assert.equal(existsSync(path), false);
        assert.equal(readFileSync(`${path}.1`, 'utf8'), 'live');
        assert.equal(readFileSync(`${path}.2`, 'utf8'), 'one');
        writeFileSync(path, 'again');
        rotateFiles(path, 0);
        assert.equal(existsSync(path), false);
    });
});

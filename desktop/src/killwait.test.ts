import assert from 'node:assert/strict';
import { EventEmitter } from 'node:events';
import { describe, it } from 'node:test';

import { killAndWait, type KillableProc } from './daemon.js';

/** A fake child process: records signals, lets the test drive `exit`. */
class FakeProc extends EventEmitter implements KillableProc {
    signals: string[] = [];
    kill(signal: 'SIGTERM' | 'SIGKILL') {
        this.signals.push(signal);
        return true;
    }
}

describe('killAndWait (th-79416c)', () => {
    it('SIGTERMs and resolves as soon as the process exits (no SIGKILL)', async () => {
        const proc = new FakeProc();
        const p = killAndWait(proc, 4000);
        // Simulate a clean exit shortly after SIGTERM.
        setImmediate(() => proc.emit('exit'));
        await p;
        assert.deepEqual(proc.signals, ['SIGTERM'], 'clean exit should not escalate to SIGKILL');
    });

    it('escalates to SIGKILL if the process does not exit within graceMs', async () => {
        const proc = new FakeProc();
        // graceMs=0 → the SIGKILL timer fires on the next tick since exit never comes.
        await killAndWait(proc, 0);
        assert.deepEqual(proc.signals, ['SIGTERM', 'SIGKILL'], 'a stuck daemon must be killed hard so the bundle frees');
    });

    it('always resolves (never hangs a quit path)', async () => {
        const proc = new FakeProc();
        // Never emit exit; graceMs small → resolves via the SIGKILL fallback.
        await assert.doesNotReject(killAndWait(proc, 5));
    });
});

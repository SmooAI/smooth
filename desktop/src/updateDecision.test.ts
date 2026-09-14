import assert from 'node:assert/strict';
import { describe, it } from 'node:test';

import {
    addSkippedVersion,
    decideAvailableAction,
    decideUpdateAction,
    MAX_INSTALL_ATTEMPTS,
    recordAttempt,
    shouldClearState,
    type UpdateState,
} from './updateDecision.js';

const gte = (a: string, b: string) => {
    const p = (s: string) => s.split('.').map((n) => Number.parseInt(n, 10) || 0);
    const [pa, pb] = [p(a), p(b)];
    for (let i = 0; i < 3; i++) {
        const d = (pa[i] ?? 0) - (pb[i] ?? 0);
        if (d !== 0) return d >= 0;
    }
    return true;
};

describe('decideUpdateAction', () => {
    const base = { downloadedVersion: '0.1.12', installedVersion: '0.1.11', promptedThisSession: false, installing: false, persisted: null };

    it('prompts to install a genuinely newer version', () => {
        assert.equal(decideUpdateAction(base), 'prompt-install');
    });

    it('ignores while an install is already in flight (guards the double-fire)', () => {
        assert.equal(decideUpdateAction({ ...base, installing: true }), 'ignore');
    });

    it('ignores a version already prompted this session (no 30-min re-nag)', () => {
        assert.equal(decideUpdateAction({ ...base, promptedThisSession: true }), 'ignore');
    });

    it('ignores when the downloaded version equals the running one', () => {
        assert.equal(decideUpdateAction({ ...base, downloadedVersion: '0.1.11' }), 'ignore');
    });

    it('gives up (manual fallback) after the version burns through its attempts', () => {
        const persisted: UpdateState = { version: '0.1.12', attempts: MAX_INSTALL_ATTEMPTS };
        assert.equal(decideUpdateAction({ ...base, persisted }), 'give-up');
    });

    it('still prompts while under the attempt cap', () => {
        const persisted: UpdateState = { version: '0.1.12', attempts: MAX_INSTALL_ATTEMPTS - 1 };
        assert.equal(decideUpdateAction({ ...base, persisted }), 'prompt-install');
    });

    it('does not carry a different version’s exhausted attempts over to a new version', () => {
        const persisted: UpdateState = { version: '0.1.11', attempts: 99 };
        assert.equal(decideUpdateAction({ ...base, persisted }), 'prompt-install');
    });
});

describe('decideAvailableAction', () => {
    const base = {
        availableVersion: '0.1.13',
        installedVersion: '0.1.12',
        skippedVersions: [] as string[],
        autoUpdate: false,
        promptedThisSession: false,
        installing: false,
        persisted: null,
    };

    it('prompts (Sparkle dialog) for a genuinely newer version', () => {
        assert.equal(decideAvailableAction(base), 'prompt');
    });

    it('auto-downloads when the user opted into automatic updates', () => {
        assert.equal(decideAvailableAction({ ...base, autoUpdate: true }), 'auto-download');
    });

    it('ignores a version the user chose to skip', () => {
        assert.equal(decideAvailableAction({ ...base, skippedVersions: ['0.1.13'] }), 'ignore');
    });

    it('ignores while an install/download is already in flight', () => {
        assert.equal(decideAvailableAction({ ...base, installing: true }), 'ignore');
    });

    it('ignores a version already offered this session (no 30-min re-nag after Remind Me Later)', () => {
        assert.equal(decideAvailableAction({ ...base, promptedThisSession: true }), 'ignore');
    });

    it('ignores when the available version equals the running one', () => {
        assert.equal(decideAvailableAction({ ...base, availableVersion: '0.1.12' }), 'ignore');
    });

    it('ignores (does not auto-download) a version that already burned through its install attempts', () => {
        const persisted: UpdateState = { version: '0.1.13', attempts: MAX_INSTALL_ATTEMPTS };
        assert.equal(decideAvailableAction({ ...base, autoUpdate: true, persisted }), 'ignore');
    });

    it('still auto-downloads while under the attempt cap', () => {
        const persisted: UpdateState = { version: '0.1.13', attempts: MAX_INSTALL_ATTEMPTS - 1 };
        assert.equal(decideAvailableAction({ ...base, autoUpdate: true, persisted }), 'auto-download');
    });

    describe('interactive ("Check for Updates…")', () => {
        it('forces the prompt even for a skipped version', () => {
            assert.equal(decideAvailableAction({ ...base, interactive: true, skippedVersions: ['0.1.13'] }), 'prompt');
        });

        it('forces the prompt even when already offered this session', () => {
            assert.equal(decideAvailableAction({ ...base, interactive: true, promptedThisSession: true }), 'prompt');
        });

        it('forces the prompt even when auto-update is on (so the user can change their mind)', () => {
            assert.equal(decideAvailableAction({ ...base, interactive: true, autoUpdate: true }), 'prompt');
        });

        it('still ignores if it is not actually a newer version', () => {
            assert.equal(decideAvailableAction({ ...base, interactive: true, availableVersion: '0.1.12' }), 'ignore');
        });

        it('still yields to an install already in flight', () => {
            assert.equal(decideAvailableAction({ ...base, interactive: true, installing: true }), 'ignore');
        });
    });
});

describe('addSkippedVersion', () => {
    it('appends a new version', () => {
        assert.deepEqual(addSkippedVersion(['0.1.11'], '0.1.13'), ['0.1.11', '0.1.13']);
    });
    it('is idempotent for an already-skipped version', () => {
        assert.deepEqual(addSkippedVersion(['0.1.13'], '0.1.13'), ['0.1.13']);
    });
    it('starts a list from empty', () => {
        assert.deepEqual(addSkippedVersion([], '0.1.13'), ['0.1.13']);
    });
});

describe('recordAttempt', () => {
    it('starts at 1 for a fresh version', () => {
        assert.deepEqual(recordAttempt(null, '0.1.12'), { version: '0.1.12', attempts: 1 });
    });
    it('increments the same version', () => {
        assert.deepEqual(recordAttempt({ version: '0.1.12', attempts: 1 }, '0.1.12'), { version: '0.1.12', attempts: 2 });
    });
    it('resets to 1 when the target version changes', () => {
        assert.deepEqual(recordAttempt({ version: '0.1.12', attempts: 5 }, '0.1.13'), { version: '0.1.13', attempts: 1 });
    });
});

describe('shouldClearState', () => {
    it('clears once the running version reaches the target (install stuck)', () => {
        assert.equal(shouldClearState({ version: '0.1.12', attempts: 2 }, '0.1.12', gte), true);
        assert.equal(shouldClearState({ version: '0.1.12', attempts: 2 }, '0.1.13', gte), true);
    });
    it('keeps state while still below the target (install failed, back on old)', () => {
        assert.equal(shouldClearState({ version: '0.1.12', attempts: 2 }, '0.1.11', gte), false);
    });
    it('nothing to clear with no state', () => {
        assert.equal(shouldClearState(null, '0.1.12', gte), false);
    });
});

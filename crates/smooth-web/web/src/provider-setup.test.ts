import assert from 'node:assert/strict';
import { test } from 'node:test';

import { byoDefaults, byoResultFrom, familyStepFrom, shouldShowCard, validateByoForm } from './provider-setup.ts';

test('the card shows only for a daemon with no model that the user has not dismissed', () => {
    assert.equal(shouldShowCard(null, false), false, 'unknown yet → render nothing');
    assert.equal(shouldShowCard({ configured: true }, false), false);
    assert.equal(shouldShowCard({ configured: false }, false), true);
    assert.equal(shouldShowCard({ configured: false }, true), false);
});

test('defaults pass validation once a key is typed', () => {
    for (const format of ['openai', 'anthropic'] as const) {
        const d = byoDefaults(format);
        assert.equal(d.format, format);
        assert.match(validateByoForm(d) ?? '', /API key/);
        assert.equal(validateByoForm({ ...d, api_key: 'sk-1' }), null);
    }
    assert.equal(byoDefaults('anthropic').base_url, 'https://api.anthropic.com/v1');
});

test('validation names the first bad field', () => {
    const ok = { format: 'openai' as const, base_url: 'https://api.example.com/v1', api_key: 'k', model: 'm' };
    assert.match(validateByoForm({ ...ok, base_url: 'api.example.com' }) ?? '', /https/);
    assert.match(validateByoForm({ ...ok, base_url: 'http://api.example.com/v1' }) ?? '', /remote endpoint/);
    assert.match(validateByoForm({ ...ok, model: 'two words' }) ?? '', /Model/);
    assert.equal(validateByoForm({ ...ok, base_url: 'http://localhost:11434/v1', api_key: '' }), null, 'local servers need no key');
    assert.equal(validateByoForm({ ...ok, base_url: 'http://192.168.1.5:1234/v1' }), null, 'LAN over http is fine');
});

test('family answers map to the next step', () => {
    assert.deepEqual(familyStepFrom({ status: 'saved', message: 'ok' }), { kind: 'saved', message: 'ok' });
    assert.deepEqual(familyStepFrom({ status: 'plan_required', upgradeUrl: 'https://smoo.ai/x', message: 'm', topUp: 'Top up' }), {
        kind: 'checkout',
        upgradeUrl: 'https://smoo.ai/x',
        message: 'm',
        topUp: 'Top up',
    });
    const older = familyStepFrom({ status: 'plan_required', upgradeUrl: 'https://smoo.ai/x' });
    assert.ok(older.kind === 'checkout' && older.topUp === '', 'a daemon without the top-up line still renders');
    const unsafe = familyStepFrom({ status: 'plan_required', upgradeUrl: 'javascript:alert(1)' });
    assert.equal(unsafe.kind, 'checkout');
    assert.ok(unsafe.kind === 'checkout' && unsafe.upgradeUrl.startsWith('https://smoo.ai/'), 'never open a non-https URL');
    assert.equal(familyStepFrom({ status: 'signed_out' }).kind, 'signin');
    assert.equal(familyStepFrom(null).kind, 'error');
});

test('byo answers say whether it saved', () => {
    assert.deepEqual(byoResultFrom(true, { status: 'configured', message: 'Saved x' }), { saved: true, message: 'Saved x' });
    assert.deepEqual(byoResultFrom(false, { status: 'probe_failed', message: '401' }), { saved: false, message: '401' });
    assert.equal(byoResultFrom(false, null).saved, false);
});

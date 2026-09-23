import assert from 'node:assert/strict';
import { test } from 'node:test';

import {
    COMPUTER_KEY,
    activeComputer,
    apiBase,
    computerNotice,
    computerRows,
    parsePeers,
    readRemembered,
    relayReason,
    resolveComputer,
    setActiveComputer,
    validDevice,
    windowTitle,
    writeRemembered,
    type PeersResponse,
} from './computers.ts';

function memoryStorage(seed: Record<string, string> = {}) {
    const m = new Map(Object.entries(seed));
    return {
        getItem: (k: string) => m.get(k) ?? null,
        setItem: (k: string, v: string) => void m.set(k, v),
        removeItem: (k: string) => void m.delete(k),
        m,
    };
}

const online: PeersResponse = {
    self: { device: 'daemon-marvin', label: 'marvin' },
    relay: { state: 'online', detail: 'Registered on the relay.' },
    peers: [{ device: 'daemon-hub', label: 'smoo-hub', kind: 'daemon' }],
    error: null,
};

const signedOut: PeersResponse = {
    self: { device: 'daemon-marvin', label: 'marvin' },
    relay: { state: 'signed_out', detail: 'Not signed in to Smoo.' },
    peers: [],
    error: 'Not signed in to Smoo.',
};

test('device ids follow the relay grammar', () => {
    assert.ok(validDevice('daemon-abc_1.2'));
    for (const bad of ['', 'a/b', '../x', 'a b', 'a:b', 'x'.repeat(65), 42, null, undefined]) assert.equal(validDevice(bad), false, String(bad));
});

test('the remembered computer survives a round trip; junk reads as this computer', () => {
    const s = memoryStorage();
    assert.equal(readRemembered(s), null);
    writeRemembered(s, { device: 'daemon-hub', label: 'smoo-hub' });
    assert.deepEqual(readRemembered(s), { device: 'daemon-hub', label: 'smoo-hub' });
    writeRemembered(s, null);
    assert.equal(s.m.has(COMPUTER_KEY), false);

    for (const junk of ['not json', '{}', '{"device":"../../etc"}', '{"device":"a/b","label":"x"}', '[]', 'null']) {
        assert.equal(readRemembered(memoryStorage({ [COMPUTER_KEY]: junk })), null, junk);
    }
    // A blank label falls back to the id rather than an empty row.
    assert.deepEqual(readRemembered(memoryStorage({ [COMPUTER_KEY]: '{"device":"daemon-hub","label":"  "}' })), { device: 'daemon-hub', label: 'daemon-hub' });
    // An invalid pick is never written.
    const t = memoryStorage();
    writeRemembered(t, { device: 'bad id', label: 'x' });
    assert.equal(t.m.has(COMPUTER_KEY), false);
});

test('storage that throws never breaks the window', () => {
    const boom = {
        getItem: () => {
            throw new Error('denied');
        },
        setItem: () => {
            throw new Error('denied');
        },
        removeItem: () => {
            throw new Error('denied');
        },
    };
    assert.equal(readRemembered(boom), null);
    assert.doesNotThrow(() => writeRemembered(boom, { device: 'daemon-hub', label: 'smoo-hub' }));
});

test('parsePeers keeps only addressable daemons other than this computer', () => {
    const parsed = parsePeers({
        self: { device: 'daemon-marvin', label: 'marvin' },
        relay: { state: 'online', detail: '' },
        peers: [
            { device: 'daemon-hub', label: 'smoo-hub', kind: 'daemon' },
            { device: 'daemon-marvin', label: 'me again', kind: 'daemon' },
            { device: 'phone-1', label: 'iPhone', kind: 'phone' },
            { device: 'daemon-hub-flow', label: 'smoo-hub · SmoothFlow', kind: 'flow' },
            { device: 'bad/id', label: 'x', kind: 'daemon' },
            { label: 'no id', kind: 'daemon' },
            { device: 'daemon-attic', label: '', kind: 'daemon' },
        ],
        error: null,
    });
    assert.deepEqual(parsed?.peers, [
        { device: 'daemon-hub', label: 'smoo-hub', kind: 'daemon' },
        { device: 'daemon-attic', label: 'daemon-attic', kind: 'daemon' },
    ]);
    for (const junk of [null, 'x', {}, { self: {} }, { self: { device: 'a', label: 'b' } }, { self: { device: 'a', label: 'b' }, relay: {} }]) {
        assert.equal(parsePeers(junk), null, JSON.stringify(junk));
    }
});

test('a remembered remote wins only while the relay lists it online', () => {
    const hub = { device: 'daemon-hub', label: 'smoo-hub' };
    assert.deepEqual(resolveComputer(null, online), { active: { kind: 'local' }, fallback: null });
    assert.deepEqual(resolveComputer(hub, online), { active: { kind: 'remote', device: 'daemon-hub', label: 'smoo-hub' }, fallback: null });

    const offline = resolveComputer(hub, { ...online, peers: [] });
    assert.deepEqual(offline.active, { kind: 'local' });
    assert.match(offline.fallback ?? '', /smoo-hub isn't reachable/);
    assert.match(offline.fallback ?? '', /Showing this computer/);

    const out = resolveComputer(hub, signedOut);
    assert.deepEqual(out.active, { kind: 'local' }, 'signed out: this computer still works');
    assert.match(out.fallback ?? '', /Sign in to Smoo/);

    const noDaemon = resolveComputer(hub, null);
    assert.deepEqual(noDaemon.active, { kind: 'local' });
    assert.match(noDaemon.fallback ?? '', /update/);
});

test('the relay reasons are human', () => {
    assert.equal(relayReason(online), null);
    assert.match(relayReason(signedOut) ?? '', /Sign in to Smoo/);
    assert.match(relayReason({ ...signedOut, relay: { state: 'session_expired', detail: '' } }) ?? '', /expired/);
    assert.match(relayReason({ ...signedOut, relay: { state: 'disabled', detail: '' } }) ?? '', /turned off/);
    assert.match(relayReason({ ...signedOut, relay: { state: 'authenticating', detail: '' } }) ?? '', /Connecting/);
    assert.equal(relayReason({ ...signedOut, relay: { state: 'offline', detail: 'Cannot reach relay.' }, error: null }), 'Cannot reach relay.');
    assert.equal(relayReason({ ...online, error: 'The Smoo Relay did not answer in time; try again.' }), 'The Smoo Relay did not answer in time; try again.');
    assert.match(relayReason(null) ?? '', /update/);
});

test('the API base tunnels through this daemon for a remote computer', () => {
    assert.equal(apiBase('http://127.0.0.1:8787/', { kind: 'local' }), 'http://127.0.0.1:8787');
    assert.equal(
        apiBase('http://127.0.0.1:8787', { kind: 'remote', device: 'daemon-hub', label: 'smoo-hub' }),
        'http://127.0.0.1:8787/api/relay/peers/daemon-hub',
    );
    // A tampered id never becomes a path.
    assert.equal(apiBase('http://127.0.0.1:8787', { kind: 'remote', device: '../../admin', label: 'x' }), 'http://127.0.0.1:8787');
});

test('switcher rows: this computer first, online remotes, and a vanished pick shown offline', () => {
    const rows = computerRows(online, { kind: 'local' }, null);
    assert.deepEqual(
        rows.map((r) => [r.device, r.name, r.online, r.active]),
        [
            [null, 'marvin', true, true],
            ['daemon-hub', 'smoo-hub', true, false],
        ],
    );

    const driving = computerRows(online, { kind: 'remote', device: 'daemon-hub', label: 'smoo-hub' }, { device: 'daemon-hub', label: 'smoo-hub' });
    assert.deepEqual(
        driving.map((r) => [r.device, r.active]),
        [
            [null, false],
            ['daemon-hub', true],
        ],
        'no duplicate row for the active + remembered computer',
    );

    const gone = computerRows(signedOut, { kind: 'local' }, { device: 'daemon-hub', label: 'smoo-hub' });
    assert.equal(gone.length, 2);
    assert.equal(gone[1].online, false);
    assert.match(gone[1].detail, /Sign in to Smoo/, 'the row says why it is unreachable');

    const noAnswer = computerRows(null, { kind: 'local' }, null);
    assert.deepEqual(
        noAnswer.map((r) => r.name),
        ['This computer'],
    );
});

test('the window title names the remote computer', () => {
    assert.equal(windowTitle('Big Smooth — your always-on AI', { kind: 'local' }), 'Big Smooth — your always-on AI');
    assert.equal(windowTitle('Big Smooth — your always-on AI', { kind: 'remote', device: 'daemon-hub', label: 'smoo-hub' }), 'Big Smooth — smoo-hub');
});

test('the active computer is module state set once at boot', () => {
    assert.deepEqual(activeComputer(), { kind: 'local' });
    setActiveComputer({ kind: 'remote', device: 'daemon-hub', label: 'smoo-hub' });
    assert.deepEqual(activeComputer(), { kind: 'remote', device: 'daemon-hub', label: 'smoo-hub' });
    assert.equal(computerNotice(), null);
    setActiveComputer({ kind: 'local' }, "smoo-hub isn't reachable");
    assert.equal(computerNotice(), "smoo-hub isn't reachable");
});

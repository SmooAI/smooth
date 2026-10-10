// The mock's replay fixture (th-7e46cd, protocol.md `flow.replay`), driven over
// a real WebSocket: `node --test apps/smoothflow/mock/`.

import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import test from 'node:test';
import { fileURLToPath } from 'node:url';

const SERVER = fileURLToPath(new URL('./server.mjs', import.meta.url));

// Start the mock on a free port with `env`; resolves to its WS URL.
async function startMock(t, env = {}) {
    const p = spawn(process.execPath, [SERVER, '0'], { env: { ...process.env, MOCK_NO_REPLAY: '', MOCK_REPLAY_PART_BYTES: '', ...env } });
    t.after(() => p.kill());
    let out = '';
    for await (const chunk of p.stdout) {
        out += chunk;
        const m = out.match(/ws:\/\/127\.0\.0\.1:(\d+)\/api\/flow\/ws/);
        if (m) return m[0];
    }
    throw new Error(`mock exited: ${out}`);
}

// A client that queues every frame and can wait for one matching `pred`.
async function connect(t, url) {
    const ws = new WebSocket(url);
    t.after(() => ws.close());
    const frames = [];
    const waiters = [];
    ws.addEventListener('message', (e) => {
        frames.push(JSON.parse(e.data));
        for (const w of waiters.splice(0)) w();
    });
    await once(ws, 'open');
    const client = {
        frames,
        send: (obj) => ws.send(JSON.stringify({ channel: 'flow', ...obj })),
        // The first frame at or after `from` matching `pred`.
        async next(pred, from = 0) {
            for (let i = 0; i < 100; i++) {
                const hit = frames.slice(from).find(pred);
                if (hit) return hit;
                await Promise.race([new Promise((r) => waiters.push(r)), new Promise((r) => setTimeout(r, 50))]);
            }
            throw new Error(`no matching frame in ${JSON.stringify(frames.slice(from))}`);
        },
        // Every frame for session `id` from `from` until things go quiet.
        async settle(id, from = 0) {
            let n = -1;
            while (n !== frames.length) {
                n = frames.length;
                await new Promise((r) => setTimeout(r, 150));
            }
            return frames.slice(from).filter((f) => f.id === id && (f.type === 'flow.output' || f.type === 'flow.replay'));
        },
    };
    await client.next((f) => f.type === 'flow.hello');
    return client;
}

const text = (f) => Buffer.from(f.data_b64 || '', 'base64').toString('utf8');
const AGENT = 'fs-1b8e05bb'; // a pty-host fixture
const TMUX = 'fs-shell001'; // the tmux-host fixture

test('hello advertises replay and every row says its host', async (t) => {
    const c = await connect(t, await startMock(t));
    const hello = c.frames[0];
    assert.deepEqual(hello.capabilities, ['replay']);
    assert.equal(hello.sessions.find((s) => s.id === AGENT).host, 'pty');
    assert.equal(hello.sessions.find((s) => s.id === TMUX).host, 'tmux');
});

test('a replay attach: covered output, the replay, then a stale output', async (t) => {
    const c = await connect(t, await startMock(t));
    const from = c.frames.length;
    c.send({ type: 'flow.attach', id: AGENT, cols: 100, rows: 30, replay: true });
    const got = await c.settle(AGENT, from);
    const at = got.findIndex((f) => f.type === 'flow.replay');
    assert.ok(at > 0, 'the seeding output arrives before the replay');
    const replay = got[at];
    assert.deepEqual([replay.cols, replay.rows, replay.reason, replay.part], [100, 30, 'attach', undefined]);
    assert.match(text(replay), /SmoothFlow mock/);
    assert.ok(
        got.slice(0, at).every((f) => f.seq <= replay.seq),
        'output before the replay is covered by it',
    );
    const after = got.slice(at + 1);
    assert.equal(after.length, 1);
    assert.equal(after[0].seq, replay.seq, 'the stale output a client must drop');

    // Typing echoes as newer output; a resize brings a replay at the new size.
    const mark = c.frames.length;
    c.send({ type: 'flow.input', id: AGENT, data_b64: Buffer.from('hi').toString('base64') });
    const echo = await c.next((f) => f.type === 'flow.output' && f.id === AGENT, mark);
    assert.ok(echo.seq > replay.seq);
    c.send({ type: 'flow.resize', id: AGENT, cols: 120, rows: 40 });
    const resized = await c.next((f) => f.type === 'flow.replay' && f.reason === 'resize', mark);
    assert.deepEqual([resized.cols, resized.rows], [120, 40]);
    assert.match(text(resized), /hi/, 'the replay carries everything so far');
});

test('seq is per session: a second client sees the same seqs', async (t) => {
    const url = await startMock(t);
    const a = await connect(t, url);
    const b = await connect(t, url);
    a.send({ type: 'flow.attach', id: AGENT, cols: 80, rows: 24, replay: true });
    const ra = await a.next((f) => f.type === 'flow.replay');
    b.send({ type: 'flow.attach', id: AGENT, cols: 80, rows: 24, replay: true });
    const rb = await b.next((f) => f.type === 'flow.replay');
    assert.equal(rb.seq, ra.seq, 'nothing new was written between the attaches');
    a.send({ type: 'flow.input', id: AGENT, data_b64: Buffer.from('x').toString('base64') });
    const oa = await a.next((f) => f.type === 'flow.output' && f.seq > ra.seq);
    const ob = await b.next((f) => f.type === 'flow.output' && f.seq > rb.seq);
    assert.equal(oa.seq, ob.seq);
});

test('a legacy attach gets the snapshot as one reset-prefixed output', async (t) => {
    const c = await connect(t, await startMock(t));
    const from = c.frames.length;
    c.send({ type: 'flow.attach', id: AGENT, cols: 80, rows: 24 });
    const got = await c.settle(AGENT, from);
    assert.ok(
        got.every((f) => f.type === 'flow.output'),
        'no replay without replay:true',
    );
    assert.ok(text(got.at(-1)).startsWith('\x1bc\x1b[3J'));
    assert.match(text(got.at(-1)), /SmoothFlow mock/);
});

test('a tmux session: an empty replay, then a newer redraw', async (t) => {
    const c = await connect(t, await startMock(t));
    const from = c.frames.length;
    c.send({ type: 'flow.attach', id: TMUX, cols: 80, rows: 24, replay: true });
    const got = await c.settle(TMUX, from);
    const at = got.findIndex((f) => f.type === 'flow.replay');
    assert.equal(got[at].data_b64, '');
    const redraw = got.slice(at + 1);
    assert.equal(redraw.length, 1);
    assert.ok(redraw[0].seq > got[at].seq, 'the redraw is newer than the replay');
    assert.match(text(redraw[0]), /SmoothFlow mock/);
});

test('MOCK_REPLAY_PART_BYTES chunks a replay into ordered parts', async (t) => {
    const c = await connect(t, await startMock(t, { MOCK_REPLAY_PART_BYTES: '16' }));
    const from = c.frames.length;
    c.send({ type: 'flow.attach', id: AGENT, cols: 80, rows: 24, replay: true });
    const parts = (await c.settle(AGENT, from)).filter((f) => f.type === 'flow.replay');
    assert.ok(parts.length > 1);
    assert.deepEqual(
        parts.map((p) => p.part),
        parts.map((_, i) => i),
    );
    assert.ok(parts.every((p) => p.parts === parts.length && p.seq === parts[0].seq && Buffer.from(p.data_b64, 'base64').length <= 16));
    const whole = Buffer.concat(parts.map((p) => Buffer.from(p.data_b64, 'base64'))).toString('utf8');
    assert.match(whole, /SmoothFlow mock/);
});

test('MOCK_NO_REPLAY=1 is an engine without the capability', async (t) => {
    const c = await connect(t, await startMock(t, { MOCK_NO_REPLAY: '1' }));
    assert.equal(c.frames[0].capabilities, undefined);
    const from = c.frames.length;
    c.send({ type: 'flow.attach', id: AGENT, cols: 80, rows: 24, replay: true });
    const got = await c.settle(AGENT, from);
    assert.ok(
        got.every((f) => f.type === 'flow.output'),
        'replay:true is ignored',
    );
});

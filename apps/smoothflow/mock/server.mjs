#!/usr/bin/env node
// Mock flow engine for developing the macOS shell without lane A.
// Zero dependencies: hand-rolled RFC 6455 server over node:http.
//
//   node apps/smoothflow/mock/server.mjs [port]      (default 8790)
//   SMOOTHFLOW_DAEMON_ADDR=127.0.0.1:8790 open SmoothFlow.app
//
// Serves flow.hello with the wireframe's fixture fleet, echoes flow.input back
// as flow.output, answers flow.approve / flow.kill / flow.new / flow.fanout.*,
// and flips states on a timer so the inbox has something to show.

import { createHash } from 'node:crypto';
import http from 'node:http';

const PORT = Number(process.argv[2] || process.env.PORT || 8790);
const now = () => new Date().toISOString();
const b64 = (s) => Buffer.from(s, 'utf8').toString('base64');

// ---------- fixtures ----------
const HOME = process.env.HOME || '/Users/me';
const mk = (id, o) => ({
    id,
    kind: 'claude',
    title: '',
    project: `${HOME}/dev/smooai/smooth`,
    worktree: `${HOME}/dev/smooai/smooth-${o.pearl_id || id}`,
    branch: o.pearl_id ? `${o.pearl_id}-${(o.title || '').split(' ')[0]}` : 'main',
    pearl_id: null,
    agent_session_id: '7f3a1c2e-0000-4000-8000-' + id.padEnd(12, '0'),
    argv: ['claude', '--resume', '7f3a'],
    tmux_session: `sf-${id}`,
    pid: 40000 + Math.floor(Math.random() * 999),
    state: 'working',
    attention: null,
    fan_out_id: null,
    created_at: now(),
    updated_at: now(),
    ended_at: null,
    exit_code: null,
    unread: false,
    ...o,
});

const sessions = new Map();
const seed = [
    mk('fs-d3e842aa', {
        pearl_id: 'th-d3e842',
        title: 'pearls → sqlite',
        state: 'needs_you',
        unread: true,
        attention: { reason: 'permission', detail: { command: 'git push -u origin th-d3e842-pearls-sqlite' }, request_id: 'req-1', resume_at: null },
    }),
    mk('fs-1b8e05bb', { pearl_id: 'th-1b8e05', title: 'claude supervisor', state: 'working' }),
    mk('fs-9483e8cc', {
        pearl_id: 'th-9483e8',
        title: 'handoff packet',
        state: 'idle',
        attention: { reason: 'held', detail: 'claude --resume 91be… is owned by pid 48122 (started 00:52, not ours)', pid: 48122 },
    }),
    mk('fs-3040aaaa', { pearl_id: 'SMOODEV-3040', title: 'relay frames', project: `${HOME}/dev/smooai/smooai`, state: 'working' }),
    mk('fs-3041bbbb', {
        pearl_id: 'SMOODEV-3041',
        title: 'labels column',
        project: `${HOME}/dev/smooai/smooai`,
        state: 'limited',
        attention: { reason: 'usage_limit', detail: 'Claude Code hit the usage limit', resume_at: new Date(Date.now() + 3600e3).toISOString() },
    }),
    mk('fs-3033cccc', {
        pearl_id: 'SMOODEV-3033',
        title: 'e2e flake',
        project: `${HOME}/dev/smooai/smooai`,
        state: 'done',
        exit_code: 0,
        ended_at: now(),
        unread: true,
    }),
    // th-883ce9: done, but its branch was never merged — flow.close refuses it
    // (nothing touched) until the shell resends with force.
    mk('fs-3034dddd', {
        pearl_id: 'SMOODEV-3034',
        title: 'unmerged branch',
        project: `${HOME}/dev/smooai/smooai`,
        state: 'done',
        exit_code: 0,
        ended_at: now(),
        unread: false,
    }),
    mk('fs-19cca5c0', { pearl_id: 'th-19cca5', title: 'fan-out C', state: 'working', fan_out_id: 'fo-1' }),
    mk('fs-shell001', {
        kind: 'shell',
        title: 'zsh · smoo-hub ssh',
        argv: ['zsh'],
        worktree: HOME,
        project: '',
        state: 'idle',
        branch: null,
        agent_session_id: null,
    }),
];
for (const s of seed) sessions.set(s.id, s);
// Sessions whose worktree the engine would refuse to remove (dirty / unmerged).
const unmerged = new Set(['fs-3034dddd']);

// flow.close (th-e126cc): why the engine would refuse, or null to go ahead.
function closeRefusal(s, m) {
    if (!m.remove_worktree || m.force || !unmerged.has(s.id)) return null;
    return `worktree ${s.worktree} has 2 uncommitted files and branch ${s.branch} is not merged into main — force to remove it anyway`;
}
// What the engine did: kill a live row, close the pearl, drop the worktree, drop the row.
function closeSession(s, m) {
    const out = { id: s.id, pearl_closed: null, worktree_removed: null, branch_deleted: null };
    if (m.close_pearl && s.pearl_id) out.pearl_closed = s.pearl_id;
    if (m.remove_worktree && s.worktree !== s.project && s.worktree !== HOME) {
        out.worktree_removed = s.worktree;
        out.branch_deleted = s.branch;
    }
    sessions.delete(s.id);
    unmerged.delete(s.id);
    broadcast({ type: 'flow.session.removed', id: s.id });
    return out;
}
const fanOuts = new Map([
    ['fo-1', { id: 'fo-1', prompt: 'pearls sync API shape', base_commit: '30ddeeb0', pearl_id: 'th-19cca5', created_at: now(), winner_session_id: null }],
]);
const fanOutCandidates = new Map([['fo-1', ['fs-19cca5c0']]]);

const handoff = (s) => ({
    pearl: {
        id: s.pearl_id,
        title: s.title || 'Replace embedded Dolt in th pearls with global SQLite',
        status: s.state === 'done' ? 'closed' : 'in_progress',
        priority: 1,
        labels: ['pearls'],
    },
    handoff: {
        worktree: s.worktree,
        branch: s.branch,
        head: 'a91c0e2',
        dirty: s.state === 'done' ? [] : ['crates/smooth-pearls/src/store.rs', 'Cargo.lock', 'docs/x.md'],
        agent_session_id: s.agent_session_id,
        next: 're-run migrate, open PR',
    },
    checkpoints: [
        { at: '2026-09-07T01:12:00Z', note: 'store.rs ported to rusqlite, 84 tests green', auto: false },
        { at: '2026-09-07T01:31:00Z', note: 'migrate-from-dolt idempotent', auto: false },
        { at: '2026-09-07T01:40:00Z', note: 'PreCompact checkpoint', auto: true },
    ],
    blocks: ['th-9483e8', 'th-19cca5'],
    pr: s.state === 'done' ? { number: 2901, url: 'https://github.com/SmooAI/smooai/pull/2901', ci: 'green' } : null,
});

// ---------- websocket plumbing ----------
const clients = new Set(); // { socket, attached: Set<id>, seq: Map }

function frame(json) {
    const payload = Buffer.from(JSON.stringify(json));
    const len = payload.length;
    let header;
    if (len < 126) header = Buffer.from([0x81, len]);
    else if (len < 65536) {
        header = Buffer.alloc(4);
        header[0] = 0x81;
        header[1] = 126;
        header.writeUInt16BE(len, 2);
    } else {
        header = Buffer.alloc(10);
        header[0] = 0x81;
        header[1] = 127;
        header.writeBigUInt64BE(BigInt(len), 2);
    }
    return Buffer.concat([header, payload]);
}

function send(c, obj) {
    if (!c.socket.destroyed) c.socket.write(frame({ channel: 'flow', ...obj }));
}
function broadcast(obj) {
    for (const c of clients) send(c, obj);
}
function sessionChanged(s, extra = {}) {
    s.updated_at = now();
    Object.assign(s, extra);
    broadcast({ type: 'flow.session', session: s });
    if (s.attention) broadcast({ type: 'flow.attention', id: s.id, attention: s.attention });
}
function output(id, text) {
    for (const c of clients) {
        if (!c.attached.has(id)) continue;
        const seq = (c.seq.get(id) || 0) + 1;
        c.seq.set(id, seq);
        send(c, { type: 'flow.output', id, seq, data_b64: b64(text) });
    }
}

function parseFrames(c, buf) {
    c.buf = c.buf ? Buffer.concat([c.buf, buf]) : buf;
    for (;;) {
        const b = c.buf;
        if (b.length < 2) return;
        const fin = b[0] & 0x80,
            op = b[0] & 0x0f,
            masked = b[1] & 0x80;
        let len = b[1] & 0x7f,
            off = 2;
        if (len === 126) {
            if (b.length < 4) return;
            len = b.readUInt16BE(2);
            off = 4;
        } else if (len === 127) {
            if (b.length < 10) return;
            len = Number(b.readBigUInt64BE(2));
            off = 10;
        }
        if (masked) off += 4;
        if (b.length < off + len) return;
        let payload = b.subarray(off, off + len);
        if (masked) {
            const m = b.subarray(off - 4, off);
            payload = Buffer.from(payload.map((x, i) => x ^ m[i % 4]));
        }
        c.buf = b.subarray(off + len);
        if (op === 0x8) {
            c.socket.end();
            return;
        }
        if (op === 0x9) {
            c.socket.write(Buffer.concat([Buffer.from([0x8a, payload.length]), payload]));
            continue;
        }
        if ((op === 0x1 || op === 0x2) && fin) {
            let msg;
            try {
                msg = JSON.parse(payload.toString('utf8'));
            } catch {
                continue;
            }
            handle(c, msg);
        }
    }
}

// ---------- protocol ----------
let counter = 0;
const newId = () => `fs-${(Date.now() + counter++).toString(16).slice(-8)}`;

function handle(c, m) {
    const s = m.id ? sessions.get(m.id) : null;
    switch (m.type) {
        case 'flow.attach': {
            if (!s) return send(c, { type: 'flow.error', ref: null, code: 'not_found', message: `no session ${m.id}` });
            c.attached.add(m.id);
            output(
                m.id,
                `\x1b[36m▐ SmoothFlow mock · ${s.pearl_id || s.title} · ${m.cols}x${m.rows}\x1b[0m\r\n$ cargo test -p smooth-pearls\r\n running 84 tests … \x1b[32mok\x1b[0m\r\n`,
            );
            if (s.attention?.reason === 'permission')
                output(
                    m.id,
                    `\x1b[33m● Bash(${s.attention.detail.command})\x1b[0m\r\n┌ Allow this command?\r\n│ [a] allow [d] deny [w] allow for session\r\n└ \r\n`,
                );
            return;
        }
        case 'flow.detach':
            c.attached.delete(m.id);
            return;
        case 'flow.input': {
            if (!s) return;
            const text = Buffer.from(m.data_b64 || '', 'base64')
                .toString('utf8')
                .replace(/\r/g, '\r\n');
            output(m.id, text);
            return;
        }
        case 'flow.resize':
            output(m.id, `\r\n\x1b[90m[resized to ${m.cols}x${m.rows}]\x1b[0m\r\n`);
            return;
        case 'flow.snapshot':
            if (s) send(c, { type: 'flow.screen', id: s.id, cols: 80, rows: 24, text: `$ (snapshot of ${s.id})` });
            return;
        case 'flow.send':
            output(m.id, `\r\n\x1b[35m> ${m.text}\x1b[0m\r\n`);
            if (s && s.state !== 'working') sessionChanged(s, { state: 'working', attention: null });
            return;
        case 'flow.approve': {
            if (!s) return;
            output(m.id, `\r\n\x1b[32m✓ ${m.decision}\x1b[0m (request ${m.request_id})\r\n`);
            if (m.decision === 'deny') sessionChanged(s, { state: 'idle', attention: null, unread: true });
            else {
                sessionChanged(s, { state: 'working', attention: null });
                setTimeout(() => {
                    output(s.id, '\r\n\x1b[32m✓ pushed. PR #2905 opened.\x1b[0m\r\n');
                    sessionChanged(s, { state: 'done', exit_code: 0, ended_at: now(), unread: true });
                }, 8000);
            }
            return;
        }
        case 'flow.kill': {
            if (!s) return;
            output(m.id, `\r\n\x1b[31m[killed pid ${s.pid}]\x1b[0m\r\n`);
            sessionChanged(
                s,
                m.resume ? { state: 'starting', attention: null, pid: 50000 + counter++ } : { state: 'dead', attention: null, exit_code: 137, ended_at: now() },
            );
            if (m.resume)
                setTimeout(() => {
                    output(s.id, '$ claude --resume ' + s.agent_session_id.slice(0, 8) + '\r\n');
                    sessionChanged(s, { state: 'working' });
                }, 1500);
            return;
        }
        case 'flow.mark_read':
            if (s) sessionChanged(s, { unread: false });
            return;
        case 'flow.close': {
            // The engine echoes the client's `seq` as `ref` on its error reply.
            const ref = m.seq ?? null;
            if (!s) return send(c, { type: 'flow.error', ref, code: 'not_found', message: `no session ${m.id}` });
            const why = closeRefusal(s, m);
            if (why) return send(c, { type: 'flow.error', ref, code: 'refused', message: why });
            closeSession(s, m);
            return;
        }
        case 'flow.new': {
            const id = newId();
            const n = mk(id, {
                kind: m.kind || 'claude',
                pearl_id: m.pearl_id,
                title: m.title || m.prompt?.slice(0, 40) || m.kind,
                state: 'starting',
                worktree: m.worktree || `${HOME}/dev/smooai/smooth-${m.pearl_id || id}`,
                argv: m.argv || ['claude', '--session-id', '…', ...(m.prompt ? [m.prompt] : [])],
            });
            sessions.set(id, n);
            sessionChanged(n);
            setTimeout(() => sessionChanged(n, { state: 'working' }), 1200);
            return;
        }
        case 'flow.fanout.new': {
            const fid = `fo-${counter++}`;
            const f = { id: fid, prompt: m.prompt, base_commit: '30ddeeb0', pearl_id: m.pearl_id, created_at: now(), winner_session_id: null };
            fanOuts.set(fid, f);
            const cands = (m.candidates || []).map((cd, i) =>
                mk(newId(), {
                    kind: cd.kind,
                    title: cd.label,
                    pearl_id: m.pearl_id ? `${m.pearl_id}-${'abc'[i] || i}` : null,
                    fan_out_id: fid,
                    state: 'working',
                }),
            );
            for (const cd of cands) sessions.set(cd.id, cd);
            fanOutCandidates.set(
                fid,
                cands.map((x) => x.id),
            );
            broadcast({ type: 'flow.fanout', fan_out: f, candidates: cands });
            cands.forEach((cd, i) => setTimeout(() => sessionChanged(cd, { state: 'done', exit_code: 0, ended_at: now(), unread: true }), 5000 + i * 4000));
            return;
        }
        case 'flow.fanout.pick': {
            const f = fanOuts.get(m.fan_out_id);
            if (!f) return;
            f.winner_session_id = m.winner_session_id;
            for (const id of fanOutCandidates.get(f.id) || []) {
                if (id === m.winner_session_id) continue;
                sessions.delete(id);
                broadcast({ type: 'flow.session.removed', id });
            }
            fanOutCandidates.set(f.id, [m.winner_session_id]);
            broadcast({ type: 'flow.fanout', fan_out: f, candidates: [sessions.get(m.winner_session_id)].filter(Boolean) });
            return;
        }
        default:
            return; // unknown types are ignored
    }
}

// ---------- timers: keep the fleet alive ----------
setInterval(() => {
    for (const s of sessions.values())
        if (s.state === 'working') output(s.id, `\x1b[90m● Edit(crates/x/${Math.random().toString(36).slice(2, 7)}.rs)\x1b[0m\r\n`);
}, 2500);
setInterval(() => {
    const s = sessions.get('fs-1b8e05bb');
    if (s && s.state === 'working')
        sessionChanged(s, {
            state: 'needs_you',
            unread: true,
            attention: { reason: 'permission', detail: { command: 'cargo publish --dry-run' }, request_id: `req-${counter++}` },
        });
}, 45000);

// ---------- phone pairing state (th-d98fde) ----------
const pendingPairs = new Map();
const pairings = new Map([
    [
        'phone-mock0000000',
        {
            device: 'phone-mock0000000',
            label: 'Brent\u2019s Pixel',
            platform: 'android',
            public_key: 'ROBtiZm7GEsCMK0Ke9PbGlaMk21hD-HPJmTq-a8asZg',
            created_at: '2026-09-01T12:00:00.000Z',
            last_seen_at: '2026-09-01T18:00:00.000Z',
        },
    ],
]);

// ---------- http ----------
const server = http.createServer((req, res) => {
    const url = new URL(req.url, 'http://x');
    const json = (code, body) => {
        res.writeHead(code, { 'content-type': 'application/json' });
        res.end(JSON.stringify(body));
    };
    let m;
    if (req.method === 'GET' && url.pathname === '/api/flow/sessions') return json(200, { sessions: [...sessions.values()] });
    if (req.method === 'GET' && (m = url.pathname.match(/^\/api\/flow\/sessions\/([^/]+)\/handoff$/))) {
        const s = sessions.get(m[1]);
        return s ? json(200, handoff(s)) : json(404, { code: 'not_found', message: m[1] });
    }
    if (req.method === 'POST' && url.pathname === '/api/flow/hooks') return json(200, {});
    // th-e126cc HTTP twin of flow.close: {id, pearl_closed, worktree_removed, branch_deleted}.
    if (req.method === 'POST' && (m = url.pathname.match(/^\/api\/flow\/sessions\/([^/]+)\/close$/))) {
        const id = m[1];
        let raw = '';
        req.on('data', (chunk) => (raw += chunk));
        req.on('end', () => {
            let body = {};
            try {
                body = raw ? JSON.parse(raw) : {};
            } catch {
                return json(400, { error: 'invalid JSON body' });
            }
            const s = sessions.get(id);
            if (!s) return json(404, { error: `no such session ${id}` });
            const why = closeRefusal(s, body);
            if (why) return json(500, { error: why });
            json(200, closeSession(s, body));
        });
        return;
    }
    // ---- phone pairing (th-d98fde): a scan "happens" on the 3rd poll ----
    if (req.method === 'POST' && url.pathname === '/api/flow/pair') {
        const id = Math.random().toString(16).slice(2, 10);
        pendingPairs.set(id, { polls: 0 });
        const code = 'EDJUdpi63P4BI0VniavN7w';
        return json(200, {
            pairing_id: id,
            url: `smoothflow://pair?v=1&p=${id}&d=daemon-mock00000000&k=hSDwCYkwp1R0i33ctD73Wg2_Og0mOBr06NxOql-OKqo&c=${code}&l=mock`,
            code,
            device: 'daemon-mock00000000',
            label: 'mock',
            daemon_public_key: 'hSDwCYkwp1R0i33ctD73Wg2_Og0mOBr06NxOql-OKqo',
            expires_at: new Date(Date.now() + 300_000).toISOString(),
            relay_enabled: true,
        });
    }
    if (req.method === 'GET' && (m = url.pathname.match(/^\/api\/flow\/pair\/([^/]+)$/))) {
        const p = pendingPairs.get(m[1]);
        if (!p) return json(200, { state: 'unknown', pairing_id: m[1] });
        p.polls += 1;
        if (p.polls < 3) return json(200, { state: 'pending', pairing_id: m[1], expires_at: new Date(Date.now() + 300_000).toISOString() });
        pendingPairs.delete(m[1]);
        const phone = {
            device: 'phone-mock0000001',
            label: 'Mock iPhone',
            platform: 'ios',
            public_key: 'd8IOd4V6q4WY1RGMlAbmaOfzsY_QyeRpttXIdD4RPtA',
            created_at: new Date().toISOString(),
            last_seen_at: new Date().toISOString(),
        };
        pairings.set(phone.device, phone);
        return json(200, { state: 'paired', pairing_id: m[1], device: phone.device, label: phone.label, platform: phone.platform });
    }
    if (req.method === 'GET' && url.pathname === '/api/flow/pairings')
        return json(200, { device: 'daemon-mock00000000', label: 'mock', relay_enabled: true, pairings: [...pairings.values()] });
    if (req.method === 'DELETE' && (m = url.pathname.match(/^\/api\/flow\/pairings\/([^/]+)$/)))
        return json(200, { device: m[1], revoked: pairings.delete(m[1]) });
    json(404, { code: 'not_found', message: url.pathname });
});

server.on('upgrade', (req, socket) => {
    if (new URL(req.url, 'http://x').pathname !== '/api/flow/ws') {
        socket.destroy();
        return;
    }
    const key = req.headers['sec-websocket-key'];
    const accept = createHash('sha1')
        .update(key + '258EAFA5-E914-47DA-95CA-C5AB0DC85B11')
        .digest('base64');
    socket.write(`HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: ${accept}\r\n\r\n`);
    const c = { socket, attached: new Set(), seq: new Map(), buf: null };
    clients.add(c);
    socket.on('data', (d) => parseFrames(c, d));
    socket.on('close', () => clients.delete(c));
    socket.on('error', () => clients.delete(c));
    send(c, { type: 'flow.hello', daemon: { version: 'mock-0.1', machine_label: 'mock' }, sessions: [...sessions.values()] });
    for (const [fid, f] of fanOuts)
        send(c, { type: 'flow.fanout', fan_out: f, candidates: (fanOutCandidates.get(fid) || []).map((id) => sessions.get(id)).filter(Boolean) });
});

server.listen(PORT, '127.0.0.1', () => console.log(`mock flow engine on ws://127.0.0.1:${PORT}/api/flow/ws`));

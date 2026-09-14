// Node test for the SmoothFlow plugin/extension overlays (pearl th-b00115):
// harness/amp/plugin.ts and harness/pi/extension.ts. Run:
//   node claude-plugins/smooth-agent/harness/flow-plugins.test.mjs
// Both are plain-JS-syntax .ts files, so Node ≥ 22.18 imports them directly.
// A fake daemon captures what each posts; a fake `amp` / `pi` API records the
// handlers each registers. No frameworks, no real CLI.
import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import http from 'node:http';
import os from 'node:os';
import path from 'node:path';

const amp = await import('./amp/plugin.ts');
const pi = await import('./pi/extension.ts');

const posted = [];
const server = http.createServer((req, res) => {
    let body = '';
    req.on('data', (c) => (body += c));
    req.on('end', () => {
        posted.push({ url: req.url, body: JSON.parse(body) });
        res.writeHead(200, { 'content-type': 'application/json' });
        res.end('{}');
    });
});
await new Promise((r) => server.listen(0, '127.0.0.1', r));
const dir = await fs.mkdtemp(path.join(os.tmpdir(), 'flow-plugins-'));
const addrFile = path.join(dir, 'daemon.addr');
await fs.writeFile(addrFile, `127.0.0.1:${server.address().port}\n`);
const waitPosts = async (n) => {
    for (let i = 0; i < 100 && posted.length < n; i++) await new Promise((r) => setTimeout(r, 20));
    assert.ok(posted.length >= n, `expected ${n} posts, got ${posted.length}`);
};

for (const mod of [amp, pi]) {
    assert.equal(mod.flowHooksUrl('127.0.0.1:9'), 'http://127.0.0.1:9/api/flow/hooks');
    assert.equal(mod.flowHooksUrl('https://h:1'), 'https://h:1/api/flow/hooks');
    assert.equal(mod.flowHooksUrl('  '), '');
    assert.equal(mod.flowHooksUrl(undefined), '');
    // No daemon / no fetch / a throwing fetch ⇒ resolves quietly.
    await mod.postFlow({}, { SMOOTH_DAEMON_ADDR_FILE: path.join(dir, 'missing') });
    await mod.postFlow({}, { SMOOTH_DAEMON_ADDR_FILE: addrFile }, null);
    await mod.postFlow({}, { SMOOTH_DAEMON_ADDR_FILE: addrFile }, () => {
        throw new Error('boom');
    });
    await mod.postFlow({}, { SMOOTH_DAEMON_ADDR_FILE: addrFile }, () => Promise.reject(new Error('down')));
}
assert.equal(posted.length, 0, 'none of the degrade paths posted');

// ── Amp: envelope table ────────────────────────────────────────────────────────
const env = { SMOOTH_FLOW_ID: 'fs-7' };
const thread = { thread: { id: 'T-abc' } };
const ampTable = [
    ['session.start', thread, { session_id: 'T-abc', payload: { hook_event_name: 'session.start', thread_id: 'T-abc' } }],
    ['agent.start', { ...thread, message: 'fix it', id: 'm1' }, { session_id: 'T-abc', payload: { hook_event_name: 'agent.start', thread_id: 'T-abc', message: 'fix it' } }],
    ['tool.result', { ...thread, tool: 'Bash', status: 'done' }, { session_id: 'T-abc', payload: { hook_event_name: 'tool.result', thread_id: 'T-abc', tool_name: 'Bash', status: 'done' } }],
    ['agent.end', { ...thread, message: 'fix it', status: 'cancelled' }, { session_id: 'T-abc', payload: { hook_event_name: 'agent.end', thread_id: 'T-abc', message: 'fix it', status: 'cancelled' } }],
    ['agent.start', undefined, { session_id: '', payload: { hook_event_name: 'agent.start', thread_id: '' } }],
];
for (const [event, ev, want] of ampTable) {
    assert.deepEqual(amp.flowBody(event, ev, env, '/wt'), { harness: 'amp', event, cwd: '/wt', flow_id: 'fs-7', ...want }, event);
}
assert.equal('flow_id' in amp.flowBody('agent.end', thread, {}, '/wt'), false, 'no SMOOTH_FLOW_ID ⇒ no flow_id');
assert.equal(amp.flowBody('agent.start', { ...thread, message: 'x'.repeat(9000) }, {}, '').payload.message.length, 4000);
assert.deepEqual(amp.FLOW_EVENTS, ['session.start', 'agent.start', 'tool.result', 'agent.end']);
assert.ok(!amp.FLOW_EVENTS.includes('tool.call'), 'tool.call needs a decision — never subscribed');

// Registration: every handler returns a neutral value and posts.
process.env.SMOOTH_DAEMON_ADDR_FILE = addrFile;
process.env.SMOOTH_FLOW_ID = 'fs-amp';
const ampHandlers = {};
amp.default({ on: (event, h) => (ampHandlers[event] = h) });
assert.deepEqual(Object.keys(ampHandlers).sort(), [...amp.FLOW_EVENTS].sort());
assert.deepEqual(ampHandlers['agent.start'](thread), {}, 'agent.start result carries no message');
assert.equal(ampHandlers['agent.end']({ ...thread, status: 'done' }), undefined);
assert.equal(ampHandlers['tool.result']({ ...thread, tool: 'Read', status: 'done' }), undefined);
await waitPosts(3);
assert.deepEqual(
    posted.map((p) => [p.url, p.body.harness, p.body.event, p.body.session_id, p.body.flow_id]).sort(),
    [
        ['/api/flow/hooks', 'amp', 'agent.end', 'T-abc', 'fs-amp'],
        ['/api/flow/hooks', 'amp', 'agent.start', 'T-abc', 'fs-amp'],
        ['/api/flow/hooks', 'amp', 'tool.result', 'T-abc', 'fs-amp'],
    ],
);
posted.length = 0;

// ── Pi: envelope table ─────────────────────────────────────────────────────────
const ctx = { cwd: '/pi/wt', sessionManager: { getSessionId: () => 'pi-sid-1' } };
assert.equal(pi.sessionIdOf(ctx), 'pi-sid-1');
assert.equal(pi.sessionIdOf({}), '');
assert.equal(pi.sessionIdOf(undefined), '');
assert.equal(pi.sessionIdOf({ sessionManager: { getSessionId: () => { throw new Error('x'); } } }), '');
assert.equal(pi.sessionIdOf({ sessionManager: { getSessionId: () => 42 } }), '');
const piTable = [
    ['session_start', { reason: 'startup' }, ctx, { session_id: 'pi-sid-1', cwd: '/pi/wt', payload: { hook_event_name: 'session_start', reason: 'startup' } }],
    ['agent_start', {}, ctx, { session_id: 'pi-sid-1', cwd: '/pi/wt', payload: { hook_event_name: 'agent_start' } }],
    ['tool_execution_start', { toolCallId: 't1', toolName: 'bash', args: {} }, ctx, { session_id: 'pi-sid-1', cwd: '/pi/wt', payload: { hook_event_name: 'tool_execution_start', tool_name: 'bash' } }],
    ['agent_end', { messages: [] }, ctx, { session_id: 'pi-sid-1', cwd: '/pi/wt', payload: { hook_event_name: 'agent_end' } }],
    ['session_shutdown', { reason: 'quit' }, ctx, { session_id: 'pi-sid-1', cwd: '/pi/wt', payload: { hook_event_name: 'session_shutdown', reason: 'quit' } }],
    ['agent_end', null, undefined, { session_id: '', cwd: '/fallback', payload: { hook_event_name: 'agent_end' } }],
];
for (const [event, ev, c, want] of piTable) {
    assert.deepEqual(pi.flowBody(event, ev, c, env, '/fallback'), { harness: 'pi', event, flow_id: 'fs-7', ...want }, event);
}
process.env.SMOOTH_FLOW_ID = 'fs-pi';
const piHandlers = {};
pi.default({ on: (event, h) => (piHandlers[event] = h) });
assert.deepEqual(Object.keys(piHandlers).sort(), [...pi.FLOW_EVENTS].sort());
for (const event of pi.FLOW_EVENTS) {
    assert.equal(piHandlers[event]({}, ctx), undefined, `${event} handler returns nothing (cannot block Pi)`);
}
await waitPosts(pi.FLOW_EVENTS.length);
assert.deepEqual(posted.map((p) => p.body.event).sort(), [...pi.FLOW_EVENTS].sort());
assert.ok(posted.every((p) => p.body.harness === 'pi' && p.body.session_id === 'pi-sid-1' && p.body.flow_id === 'fs-pi'));

server.close();
await fs.rm(dir, { recursive: true, force: true });
console.log('flow plugins (amp, pi): ok');

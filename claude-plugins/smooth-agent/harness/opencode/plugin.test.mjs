// Node smoke test for the smooth-agent OpenCode plugin (pearl th-cc50cd).
// Run: node smooth-agent.test.mjs — asserts the lifecycle without OpenCode:
// register on session.created, throttled working heartbeat, idle, offline,
// and the th-missing degrade path. No frameworks.
import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import http from 'node:http';
import os from 'node:os';
import path from 'node:path';

import { SmoothAgent, flowHooksUrl } from './plugin.js';

// SmoothFlow hooks (th-5c5457): a fake daemon captures what the plugin posts.
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
const addrFile = path.join(await fs.mkdtemp(path.join(os.tmpdir(), 'flow-addr-')), 'daemon.addr');
await fs.writeFile(addrFile, `127.0.0.1:${server.address().port}\n`);
process.env.SMOOTH_DAEMON_ADDR_FILE = addrFile;
assert.equal(await flowHooksUrl(addrFile), `http://127.0.0.1:${server.address().port}/api/flow/hooks`);
assert.equal(await flowHooksUrl('/nonexistent/daemon.addr'), '', 'no daemon advertised ⇒ no URL, no throw');

const calls = [];
let fail = false;
// Bun-shell stand-in: a tag function whose single interpolation is the argv array.
const $ = (_strings, args) => ({
    quiet: () => (fail ? Promise.reject(new Error('no th')) : (calls.push(args.join(' ')), Promise.resolve())),
});

const plugin = await SmoothAgent({ $, directory: '/Users/x/dev/My Repo' });
const sid = { sessionID: 'ses_abcd1234' };

await plugin['session.created'](sid);
assert.equal(calls.length, 1);
assert.match(calls[0], /^agent register --name oc-myrepo-1234 --harness opencode --pid \d+$/);

// Unknown session shapes are skipped, never crash.
await plugin['session.created']({});
await plugin['session.idle']({ sessionID: 'never-registered' });
assert.equal(calls.length, 1);

// tool activity → working, throttled to one call per window.
await plugin['tool.execute.before'](sid);
await plugin['tool.execute.before'](sid);
assert.equal(calls.length, 2);
assert.equal(calls[1], 'agent status --name oc-myrepo-1234 --status working');

await plugin['session.idle'](sid);
assert.equal(calls[2], 'agent status --name oc-myrepo-1234 --status idle');
// idle resets the throttle so the next activity re-marks working immediately.
await plugin['tool.execute.before'](sid);
assert.equal(calls[3], 'agent status --name oc-myrepo-1234 --status working');

await plugin['session.deleted'](sid);
assert.equal(calls[4], 'agent status --name oc-myrepo-1234 --status offline');
// After deletion the session is forgotten.
await plugin['tool.execute.before'](sid);
assert.equal(calls.length, 5);

// th failure degrades to silence, permanently, without throwing.
const plugin2 = await SmoothAgent({ $, directory: '/tmp/z' });
fail = true;
await plugin2['session.created']({ sessionID: 'ses_zzzz9999' });
fail = false;
const before = calls.length;
await plugin2['tool.execute.before']({ sessionID: 'ses_zzzz9999' });
assert.equal(calls.length, before, 'after one failure the plugin stays silent');

// The flow engine saw the same lifecycle as Claude Code hook events, with the
// harness name, the opencode session id and the cwd it must bind by.
await new Promise((r) => setTimeout(r, 200));
const events = posted.map((p) => p.body.event);
// Every tool call posts (the Chat tab wants each one); only the th-mail
// heartbeat above is throttled.
assert.deepEqual(events.slice(0, 6), ['SessionStart', 'PreToolUse', 'PreToolUse', 'Stop', 'PreToolUse', 'SessionEnd'], JSON.stringify(events));
assert.ok(posted.every((p) => p.url === '/api/flow/hooks'));
const first = posted[0].body;
assert.equal(first.harness, 'opencode');
assert.equal(first.session_id, 'ses_abcd1234');
assert.equal(first.cwd, '/Users/x/dev/My Repo');
assert.deepEqual(posted[1].body.payload, { tool_name: 'tool', tool_input: {} });
server.close();

console.log('ok — smooth-agent opencode plugin lifecycle');

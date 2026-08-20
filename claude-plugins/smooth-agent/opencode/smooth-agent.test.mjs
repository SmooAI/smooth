// Node smoke test for the smooth-agent OpenCode plugin (pearl th-cc50cd).
// Run: node smooth-agent.test.mjs — asserts the lifecycle without OpenCode:
// register on session.created, throttled working heartbeat, idle, offline,
// and the th-missing degrade path. No frameworks.
import assert from "node:assert/strict";
import { SmoothAgent } from "./smooth-agent.js";

const calls = [];
let fail = false;
// Bun-shell stand-in: a tag function whose single interpolation is the argv array.
const $ = (_strings, args) => ({
    quiet: () => (fail ? Promise.reject(new Error("no th")) : (calls.push(args.join(" ")), Promise.resolve())),
});

const plugin = await SmoothAgent({ $, directory: "/Users/x/dev/My Repo" });
const sid = { sessionID: "ses_abcd1234" };

await plugin["session.created"](sid);
assert.equal(calls.length, 1);
assert.match(calls[0], /^agent register --name oc-myrepo-1234 --harness opencode --pid \d+$/);

// Unknown session shapes are skipped, never crash.
await plugin["session.created"]({});
await plugin["session.idle"]({ sessionID: "never-registered" });
assert.equal(calls.length, 1);

// tool activity → working, throttled to one call per window.
await plugin["tool.execute.before"](sid);
await plugin["tool.execute.before"](sid);
assert.equal(calls.length, 2);
assert.equal(calls[1], "agent status --name oc-myrepo-1234 --status working");

await plugin["session.idle"](sid);
assert.equal(calls[2], "agent status --name oc-myrepo-1234 --status idle");
// idle resets the throttle so the next activity re-marks working immediately.
await plugin["tool.execute.before"](sid);
assert.equal(calls[3], "agent status --name oc-myrepo-1234 --status working");

await plugin["session.deleted"](sid);
assert.equal(calls[4], "agent status --name oc-myrepo-1234 --status offline");
// After deletion the session is forgotten.
await plugin["tool.execute.before"](sid);
assert.equal(calls.length, 5);

// th failure degrades to silence, permanently, without throwing.
const plugin2 = await SmoothAgent({ $, directory: "/tmp/z" });
fail = true;
await plugin2["session.created"]({ sessionID: "ses_zzzz9999" });
fail = false;
const before = calls.length;
await plugin2["tool.execute.before"]({ sessionID: "ses_zzzz9999" });
assert.equal(calls.length, before, "after one failure the plugin stays silent");

console.log("ok — smooth-agent opencode plugin lifecycle");

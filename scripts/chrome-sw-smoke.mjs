// Boots the extension's service worker (background.js) against a stub `chrome` that has only the
// up-front permissions, the state of a fresh install. Optional APIs (debugger, tabGroups,
// scripting) are absent until granted, exactly as in Chrome; touching one at top level throws
// and kills the worker, which once left the extension unable to connect to th at all.
// Usage: node scripts/chrome-sw-smoke.mjs
import { readFileSync } from 'node:fs';
import vm from 'node:vm';

const dir = new URL('../crates/smooth-cli/src/chrome/extension/', import.meta.url);
const read = (f) => readFileSync(new URL(f, dir), 'utf8');

const listeners = {};
const event = (name) => ({
    addListener: (fn) => (listeners[name] ??= []).push(fn),
    removeListener: () => {},
});
const port = { onMessage: event('port.onMessage'), onDisconnect: event('port.onDisconnect'), postMessage: () => {}, disconnect: () => {} };
const granted = new Set();
const chrome = {
    runtime: {
        getManifest: () => JSON.parse(read('manifest.json')),
        getPlatformInfo: async () => ({ os: 'mac' }),
        connectNative: () => port,
        onMessage: event('runtime.onMessage'),
        onStartup: event('runtime.onStartup'),
        onInstalled: event('runtime.onInstalled'),
        lastError: undefined,
    },
    // `tabs` events and methods exist without the "tabs" permission (it only gates url/title).
    tabs: { onRemoved: event('tabs.onRemoved'), onCreated: event('tabs.onCreated'), onUpdated: event('tabs.onUpdated') },
    bookmarks: Object.fromEntries(
        ['onCreated', 'onRemoved', 'onChanged', 'onMoved', 'onChildrenReordered', 'onImportEnded'].map((e) => [e, event(`bookmarks.${e}`)]),
    ),
    permissions: {
        contains: async (p) => (p.permissions ?? []).every((x) => granted.has(x)),
        onAdded: event('permissions.onAdded'),
        onRemoved: event('permissions.onRemoved'),
    },
    alarms: { onAlarm: event('alarms.onAlarm'), create: () => {} },
    action: { setBadgeText: async () => {}, setBadgeBackgroundColor: async () => {} },
};

const ctx = vm.createContext({ chrome, console, setTimeout, clearTimeout, setInterval, clearInterval, URL, TextEncoder, TextDecoder });
ctx.self = ctx;
ctx.globalThis = ctx;
ctx.importScripts = (...files) => files.forEach((f) => vm.runInContext(read(f), ctx, { filename: f }));

const checks = {};
try {
    vm.runInContext(read('background.js'), ctx, { filename: 'background.js' });
    checks['worker boots without optional permissions'] = true;
} catch (e) {
    checks['worker boots without optional permissions'] = false;
    console.error(e.stack);
}
checks['popup status listener registered'] = (listeners['runtime.onMessage'] ?? []).length > 0;
checks['grant listener registered'] = (listeners['permissions.onAdded'] ?? []).length > 0;

// Granting trusted input: debugger appears, and its listeners attach then (once).
granted.add('debugger');
chrome.debugger = { onEvent: event('debugger.onEvent'), onDetach: event('debugger.onDetach') };
for (const fn of listeners['permissions.onAdded'] ?? []) await fn({ permissions: ['debugger'] });
for (const fn of listeners['permissions.onAdded'] ?? []) await fn({ permissions: ['debugger'] });
checks['debugger listeners attach on grant, once'] = listeners['debugger.onEvent']?.length === 1 && listeners['debugger.onDetach']?.length === 1;

const failed = Object.entries(checks).filter(([, ok]) => !ok);
for (const [name, ok] of Object.entries(checks)) console.log(`${ok ? 'ok  ' : 'FAIL'} ${name}`);
if (failed.length) process.exit(1);

// Boots the extension's service worker (background.js) against a stub `chrome` that has only the
// up-front permissions, the state of a fresh install. Optional APIs (tabGroups, scripting) are
// absent until granted, exactly as in Chrome, and a browser that drops a permission leaves its API
// undefined; touching one at top level throws and kills the worker, which once left the extension
// unable to connect to th at all. Trusted input is a stored setting, gated on browsing.
// Usage: node scripts/chrome-sw-smoke.mjs
import { readFileSync } from 'node:fs';
import vm from 'node:vm';

const dir = new URL('../crates/smooth-cli/src/chrome/extension/', import.meta.url);
const read = (f) => readFileSync(new URL(f, dir), 'utf8');

function boot({ withDebugger }) {
    const listeners = {};
    const event = (name) => ({
        addListener: (fn) => (listeners[name] ??= []).push(fn),
        removeListener: () => {},
    });
    const fire = async (name, ...args) => {
        for (const fn of listeners[name] ?? []) await fn(...args);
    };
    const port = { onMessage: event('port.onMessage'), onDisconnect: event('port.onDisconnect'), postMessage: () => {}, disconnect: () => {} };
    const granted = new Set();
    const store = {};
    const detached = [];
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
            contains: async (p) => [...(p.permissions ?? []), ...(p.origins ?? [])].every((x) => granted.has(x)),
            onAdded: event('permissions.onAdded'),
            onRemoved: event('permissions.onRemoved'),
        },
        storage: {
            local: {
                get: async (k) => (k in store ? { [k]: store[k] } : {}),
                set: async (o) => {
                    Object.assign(store, o);
                    await fire('storage.onChanged', Object.fromEntries(Object.entries(o).map(([k, v]) => [k, { newValue: v }])), 'local');
                },
            },
            onChanged: event('storage.onChanged'),
        },
        alarms: { onAlarm: event('alarms.onAlarm'), create: () => {} },
        action: { setBadgeText: async () => {}, setBadgeBackgroundColor: async () => {} },
    };
    if (withDebugger) {
        chrome.debugger = { onEvent: event('debugger.onEvent'), onDetach: event('debugger.onDetach'), detach: async (t) => detached.push(t.tabId) };
    }
    const ctx = vm.createContext({ chrome, console, setTimeout, clearTimeout, setInterval, clearInterval, URL, TextEncoder, TextDecoder });
    ctx.self = ctx;
    ctx.globalThis = ctx;
    ctx.importScripts = (...files) => files.forEach((f) => vm.runInContext(read(f), ctx, { filename: f }));
    let error = null;
    try {
        vm.runInContext(read('background.js'), ctx, { filename: 'background.js' });
    } catch (e) {
        error = e;
        console.error(e.stack);
    }
    const run = (src) => vm.runInContext(src, ctx);
    return { listeners, fire, granted, detached, error, run, chrome };
}

const checks = {};

// A browser that drops `debugger` (as Chrome does when it is listed as optional).
const bare = boot({ withDebugger: false });
checks['worker boots with chrome.debugger missing'] = !bare.error;
checks['popup status listener registered'] = (bare.listeners['runtime.onMessage'] ?? []).length > 0;

// The real install: debugger declared up front, browsing not yet granted.
const w = boot({ withDebugger: true });
checks['worker boots with debugger declared'] = !w.error;
checks['debugger listeners attach once'] = w.listeners['debugger.onEvent']?.length === 1 && w.listeners['debugger.onDetach']?.length === 1;
const status = () => w.run("METHODS['trusted.status']()").then((r) => r.enabled);
checks['trusted input off by default'] = (await status()) === false;
await w.chrome.storage.local.set({ trusted: true });
checks['trusted input needs browsing'] = (await status()) === false;
['tabs', 'scripting', 'tabGroups', '<all_urls>'].forEach((p) => w.granted.add(p));
await w.fire('permissions.onAdded', { permissions: ['tabs'] });
checks['trusted input on with browsing + setting'] = (await status()) === true;
// A live session, then the switch goes off: the session is detached and forgotten.
w.run('dbg.set(7, { idle: null })');
await w.chrome.storage.local.set({ trusted: false });
await new Promise((r) => setTimeout(r, 0));
checks['turning it off detaches every session'] = w.detached.includes(7) && w.run('dbg.size') === 0 && (await status()) === false;

const failed = Object.entries(checks).filter(([, ok]) => !ok);
for (const [name, ok] of Object.entries(checks)) console.log(`${ok ? 'ok  ' : 'FAIL'} ${name}`);
if (failed.length) process.exit(1);

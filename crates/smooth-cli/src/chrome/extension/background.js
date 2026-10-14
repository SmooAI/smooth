importScripts('cdp.js');

const HOST = 'ai.smoo.smooth_chrome';
const BROWSING = { permissions: ['tabs', 'scripting', 'tabGroups'], origins: ['<all_urls>'] };
// The opt-in "trusted input" tier: chrome.debugger, so clicks and keys are real input events.
// Chrome refuses `debugger` as an optional permission (it silently drops it), so it is declared
// up front and this setting, off by default and only on while browsing is granted, gates every attach.
const TRUSTED_KEY = 'trusted';

let port = null;
let backoff = 1000;
let lastError = null;

function connect() {
    if (port) return;
    try {
        port = chrome.runtime.connectNative(HOST);
    } catch (e) {
        lastError = String(e);
        return retry();
    }
    port.onMessage.addListener(onRequest);
    port.onDisconnect.addListener(() => {
        lastError = chrome.runtime.lastError?.message ?? 'disconnected';
        port = null;
        retry();
    });
    backoff = 1000;
    lastError = null;
    hello();
}

// A pending timer doesn't survive the worker being stopped; the alarm below does.
function retry() {
    setTimeout(connect, backoff);
    backoff = Math.min(backoff * 2, 60000);
}

async function hello() {
    port?.postMessage({ type: 'hello', version: chrome.runtime.getManifest().version, browsing: await browsingEnabled(), trusted: await trustedEnabled() });
}

const browsingEnabled = () => chrome.permissions.contains(BROWSING);
const trustedEnabled = async () => (await chrome.storage.local.get(TRUSTED_KEY))[TRUSTED_KEY] === true && (await browsingEnabled());
const platform = chrome.runtime.getPlatformInfo();

async function onRequest(msg) {
    if (msg?.id === undefined) return;
    try {
        const handler = METHODS[msg.method];
        if (!handler) throw new Error(`unknown method ${msg.method}`);
        port?.postMessage({ id: msg.id, result: (await handler(msg.params ?? {})) ?? null });
    } catch (e) {
        port?.postMessage({ id: msg.id, error: { message: e?.message ?? String(e) } });
    }
}

async function needBrowsing() {
    if (!(await browsingEnabled())) throw new Error('browsing is off: click the Smooth extension icon and choose Enable browsing');
}

function waitForLoad(tabId, timeoutMs = 30000) {
    return new Promise((resolve) => {
        const done = () => {
            chrome.tabs.onUpdated.removeListener(listener);
            clearTimeout(timer);
            chrome.tabs.get(tabId).then(resolve, () => resolve(null));
        };
        const listener = (id, info) => id === tabId && info.status === 'complete' && done();
        const timer = setTimeout(done, timeoutMs);
        chrome.tabs.onUpdated.addListener(listener);
        chrome.tabs.get(tabId).then((t) => t.status === 'complete' && done(), done);
    });
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

// Chrome's schema validation rejects null for optional fields; drop them.
const clean = (o) => Object.fromEntries(Object.entries(o).filter(([, v]) => v !== null && v !== undefined));

const tabInfo = (t) => t && { tabId: t.id, windowId: t.windowId, url: t.url, title: t.title, active: t.active, status: t.status };

// Refs from subframes carry their frame: "f12:e3".
function parseRef(ref) {
    const m = /^f(\d+):(.+)$/.exec(ref ?? '');
    return m ? { frameId: Number(m[1]), ref: m[2] } : { frameId: 0, ref };
}

async function inPage(tabId, op, args = {}, frameId = 0, timeoutMs = 20000) {
    await needBrowsing();
    const run = chrome.scripting.executeScript({ target: { tabId, frameIds: [frameId] }, func: pageCall, args: [op, args] });
    const timeout = sleep(timeoutMs).then(() => {
        const d = dbg.get(tabId)?.dialog;
        if (d) throw new Error(`the page is blocked by a ${d.type} dialog ("${d.message}"): answer it with browse_dialog`);
        throw new Error(
            'the page is not responding: a JavaScript dialog (alert/confirm) may be open; enable trusted input to answer it with browse_dialog, or ask the user to dismiss it',
        );
    });
    const [res] = await Promise.race([run, timeout]);
    return res?.result;
}

async function snapshot(tabId, limit = 400) {
    await needBrowsing();
    const results = await chrome.scripting.executeScript({ target: { tabId, allFrames: true }, func: pageCall, args: ['snapshot', { limit }] });
    const top = results.find((r) => r.frameId === 0)?.result ?? { elements: [] };
    top.frames = results
        .filter((r) => r.frameId !== 0 && r.result?.elements?.length)
        .map((r) => ({
            frameId: r.frameId,
            url: r.result.url,
            truncated: r.result.truncated,
            elements: r.result.elements.map((e) => (e.ref ? { ...e, ref: `f${r.frameId}:${e.ref}` } : e)),
        }));
    return top;
}

let acting = 0;
async function badge(on) {
    acting += on ? 1 : -1;
    await chrome.action.setBadgeText({ text: acting > 0 ? 'AI' : '' }).catch(() => {});
}

// ── trusted input: chrome.debugger (optional permission) ───────────────────────────────
// One debugger session per tab, attached on first use (single-flight), detached after
// IDLE_MS without a command so Chrome's "is debugging this browser" bar goes away, kept
// while console/network capture is on. Every command has a timeout, and a command that
// finds the session gone re-attaches once.

const ATTACH_MS = 5000;
const COMMAND_MS = 15000;
const IDLE_MS = 30000;
const CONSOLE_MAX = 1000;
const NETWORK_MAX = 500;
const dbg = new Map(); // tabId → { idle, dialog, console: [] | null, network: Map | null }
const attaching = new Map();

const withTimeout = (p, ms, msg) =>
    Promise.race([
        p,
        sleep(ms).then(() => {
            throw new Error(msg);
        }),
    ]);

async function needTrusted() {
    await needBrowsing();
    if (!(await trustedEnabled())) throw new Error('trusted input is off: click the Smooth extension icon and turn on Trusted input');
}

function attach(tabId) {
    if (dbg.has(tabId)) return Promise.resolve(dbg.get(tabId));
    if (!attaching.has(tabId))
        attaching.set(
            tabId,
            attachNow(tabId).finally(() => attaching.delete(tabId)),
        );
    return attaching.get(tabId);
}

async function attachNow(tabId) {
    const url = (await chrome.tabs.get(tabId)).url ?? '';
    if (/^(chrome|chrome-extension|chrome-untrusted|devtools|edge):/.test(url)) throw new Error(`browser pages are off limits to the debugger: ${url}`);
    listenDebugger();
    // A session left over from a crashed worker would make attach fail.
    await chrome.debugger.detach({ tabId }).catch(() => {});
    try {
        await withTimeout(chrome.debugger.attach({ tabId }, '1.3'), ATTACH_MS, 'attaching the debugger timed out (has the renderer crashed?)');
    } catch (e) {
        if (/already attached|another debugger/i.test(String(e?.message ?? e))) {
            throw new Error('another debugger is attached to this tab (DevTools, or another extension): close it, or call without trusted');
        }
        throw e;
    }
    const s = { idle: null, dialog: null, console: null, network: null };
    dbg.set(tabId, s);
    // Page.enable reports JavaScript dialogs (including one already open), so browse_dialog can answer them.
    await chrome.debugger.sendCommand({ tabId }, 'Page.enable');
    // Background tabs behave as focused, so focus-sensitive UIs work without stealing the window.
    await chrome.debugger.sendCommand({ tabId }, 'Emulation.setFocusEmulationEnabled', { enabled: true }).catch(() => {});
    return s;
}

function touch(tabId, s) {
    clearTimeout(s.idle);
    if (!s.console && !s.network) s.idle = setTimeout(() => chrome.debugger.detach({ tabId }).catch(() => {}), IDLE_MS);
}

async function session(tabId) {
    await needTrusted();
    const s = await attach(tabId);
    touch(tabId, s);
    return s;
}

async function cdp(tabId, method, params = {}) {
    for (let attempt = 0; ; attempt++) {
        await session(tabId);
        try {
            return await withTimeout(
                chrome.debugger.sendCommand({ tabId }, method, params),
                COMMAND_MS,
                `${method} timed out: the page may be frozen, or blocked by a dialog (browse_dialog)`,
            );
        } catch (e) {
            if (attempt === 0 && /not attached|detached/i.test(String(e?.message ?? e))) {
                dbg.delete(tabId);
                continue;
            }
            throw e;
        }
    }
}

const argText = (a) =>
    a.value !== undefined ? (typeof a.value === 'string' ? a.value : JSON.stringify(a.value)) : (a.unserializableValue ?? a.description ?? a.type);
const capped = (arr, max, item) => {
    arr.push(item);
    if (arr.length > max) arr.splice(0, arr.length - max);
};

function onDebuggerEvent({ tabId }, method, p) {
    const s = dbg.get(tabId);
    if (!s) return;
    switch (method) {
        case 'Page.javascriptDialogOpening':
            s.dialog = { type: p.type, message: p.message, defaultPrompt: p.defaultPrompt, url: p.url };
            break;
        case 'Page.javascriptDialogClosed':
            s.dialog = null;
            break;
        case 'Runtime.consoleAPICalled':
            if (s.console) {
                const f = p.stackTrace?.callFrames?.[0];
                capped(s.console, CONSOLE_MAX, {
                    level: p.type,
                    text: p.args.map(argText).join(' ').slice(0, 4000),
                    source: f ? `${f.url}:${f.lineNumber + 1}` : undefined,
                    at: p.timestamp,
                });
            }
            break;
        case 'Runtime.exceptionThrown':
            if (s.console) {
                const d = p.exceptionDetails;
                capped(s.console, CONSOLE_MAX, {
                    level: 'exception',
                    text: (d.exception?.description ?? d.text).slice(0, 4000),
                    source: d.url ? `${d.url}:${d.lineNumber + 1}` : undefined,
                    at: p.timestamp,
                });
            }
            break;
        case 'Network.requestWillBeSent':
            if (s.network) {
                s.network.set(p.requestId, { method: p.request.method, url: p.request.url, type: p.type, at: p.wallTime });
                if (s.network.size > NETWORK_MAX) s.network.delete(s.network.keys().next().value);
            }
            break;
        case 'Network.responseReceived': {
            const r = s.network?.get(p.requestId);
            if (r) Object.assign(r, { status: p.response.status, mime: p.response.mimeType });
            break;
        }
        case 'Network.loadingFailed': {
            const r = s.network?.get(p.requestId);
            if (r) r.failed = p.canceled ? 'canceled' : p.errorText;
            break;
        }
    }
}

function onDebuggerDetach({ tabId }) {
    clearTimeout(dbg.get(tabId)?.idle);
    dbg.delete(tabId);
}

// A throw at top level kills the whole service worker (no host connection, no popup status),
// so never assume chrome.debugger exists: a browser that drops the permission leaves it undefined.
let debuggerListening = false;
function listenDebugger() {
    if (debuggerListening || !chrome.debugger) return;
    chrome.debugger.onEvent.addListener(onDebuggerEvent);
    chrome.debugger.onDetach.addListener(onDebuggerDetach);
    debuggerListening = true;
}
listenDebugger();
chrome.tabs.onRemoved.addListener((tabId) => dbg.delete(tabId));

async function viewport(tabId) {
    const m = await cdp(tabId, 'Page.getLayoutMetrics');
    const v = m.cssVisualViewport ?? m.visualViewport;
    return { width: v.clientWidth, height: v.clientHeight, metrics: m };
}

// A click/type/key/hover/scroll/upload as real input through the debugger. Targets are a ref
// (main frame) or x/y in CSS pixels, the coordinates of a browse_screenshot image.
async function trustedAct(tabId, op, args) {
    const mac = (await platform).os === 'mac';
    const mouse = async (events) => {
        for (const e of events) await cdp(tabId, 'Input.dispatchMouseEvent', e);
    };
    const keys = async (events) => {
        for (const e of events) await cdp(tabId, 'Input.dispatchKeyEvent', e);
    };
    const mainFrameRef = () => {
        const { frameId, ref } = parseRef(args.ref);
        if (frameId !== 0) throw new Error('trusted input by ref works in the main frame only: pass x/y from browse_screenshot instead');
        return ref;
    };
    const target = async () => {
        await session(tabId);
        if (args.ref) return inPage(tabId, 'center', { ref: mainFrameRef() });
        const vp = await viewport(tabId);
        SmoothCdp.checkPoint(args.x, args.y, vp.width, vp.height);
        return { x: args.x, y: args.y };
    };
    const where = (p) =>
        `${args.ref ?? ''} at (${Math.round(p.x)}, ${Math.round(p.y)})${p.covered ? `; it is covered by ${p.covered}, which got the event` : ''}`;
    switch (op) {
        case 'click': {
            const p = await target();
            await mouse(SmoothCdp.clickEvents(p.x, p.y, { button: args.button ?? 'left', count: args.count ?? 1 }));
            return `clicked${args.count > 1 ? ` ×${args.count}` : ''} ${where(p)} (trusted)`;
        }
        case 'hover': {
            const p = await target();
            await mouse(SmoothCdp.hoverEvents(p.x, p.y));
            return `hovered ${where(p)} (trusted)`;
        }
        case 'type': {
            await session(tabId);
            if (args.ref) await inPage(tabId, 'focusAll', { ref: mainFrameRef() });
            else if (args.x !== undefined) {
                const p = await target();
                await mouse(SmoothCdp.clickEvents(p.x, p.y));
            }
            if (args.text === '') await keys(SmoothCdp.keyEvents('Backspace', { mac }));
            for (const [method, params] of SmoothCdp.typeCalls(args.text, { mac })) await cdp(tabId, method, params);
            if (args.submit) await keys(SmoothCdp.keyEvents('Enter', { mac }));
            return 'typed (trusted)';
        }
        case 'key': {
            await session(tabId);
            if (args.ref) await inPage(tabId, 'focus', { ref: mainFrameRef() });
            const times = Math.max(1, Math.min(100, args.repeat ?? 1));
            for (let i = 0; i < times; i++) await keys(SmoothCdp.keyEvents(args.key, { mac }));
            return `pressed ${args.key}${times > 1 ? ` ×${times}` : ''} (trusted)`;
        }
        case 'scroll': {
            const vp = await viewport(tabId);
            const p = args.x !== undefined ? await target() : { x: vp.width / 2, y: vp.height / 2 };
            const dy = (args.direction === 'up' ? -1 : 1) * vp.height * 0.9 * (args.pages ?? 1);
            await mouse(SmoothCdp.wheelEvents(p.x, p.y, 0, dy));
            return `scrolled ${args.direction ?? 'down'} (trusted)`;
        }
        case 'upload': {
            if (!args.paths?.length) throw new Error('give at least one file path');
            const mark = `u${Date.now()}`;
            await session(tabId);
            const marked = await inPage(tabId, 'mark', { ref: args.ref ? mainFrameRef() : undefined, mark });
            try {
                const { root } = await cdp(tabId, 'DOM.getDocument', { depth: 0 });
                const { nodeId } = await cdp(tabId, 'DOM.querySelector', { nodeId: root.nodeId, selector: `[data-smooth-upload="${mark}"]` });
                if (!nodeId) throw new Error('the debugger cannot reach that file input (inside a shadow root?)');
                // Fires the input's input/change events like a real file picker.
                await cdp(tabId, 'DOM.setFileInputFiles', { files: args.paths, nodeId });
            } finally {
                await inPage(tabId, 'unmark', { mark }).catch(() => {});
            }
            return `attached ${args.paths.length} file(s) to ${marked}`;
        }
        default:
            throw new Error(`${op} has no trusted version; call it without trusted`);
    }
}

// Run an in-page action, then wait for a navigation or for the DOM to settle,
// and report what changed (new URL, tabs it opened).
async function act(tabId, op, args) {
    await needBrowsing();
    const { frameId, ref } = parseRef(args.ref);
    const before = await chrome.tabs.get(tabId);
    const opened = [];
    const onCreated = (t) => t.openerTabId === tabId && opened.push(t.id);
    chrome.tabs.onCreated.addListener(onCreated);
    await badge(true);
    try {
        let result;
        try {
            result =
                args.trusted || args.x !== undefined || op === 'upload'
                    ? await trustedAct(tabId, op, args)
                    : await inPage(tabId, op, { ...args, ref }, frameId);
        } catch (e) {
            // A click that navigates can tear the frame down under us; that's success.
            if ((await chrome.tabs.get(tabId)).status !== 'loading') throw e;
        }
        await sleep(150);
        let t = await chrome.tabs.get(tabId);
        if (t.status === 'loading') t = await waitForLoad(tabId);
        else await inPage(tabId, 'settle', {}, frameId).catch(() => {});
        return { result, navigated: t?.url !== before.url, url: t?.url, title: t?.title, opened };
    } finally {
        chrome.tabs.onCreated.removeListener(onCreated);
        await badge(false);
    }
}

// Agent-opened tabs go in a "Smooth" group so the user can see them.
async function groupTab(tab) {
    try {
        const [g] = await chrome.tabGroups.query({ title: 'Smooth', windowId: tab.windowId });
        const groupId = await chrome.tabs.group(clean({ tabIds: [tab.id], groupId: g?.id, createProperties: g ? undefined : { windowId: tab.windowId } }));
        if (!g) await chrome.tabGroups.update(groupId, { title: 'Smooth', color: 'purple' });
    } catch {
        // Cosmetic only.
    }
}

// Runs inside the page, in the extension's isolated world. It must be self-contained:
// executeScript serializes only this function. State lives on the isolated world's global,
// so refs survive between calls and the page can't see or clone them.
async function pageCall(op, a) {
    const S = (globalThis.__smooth ??= { n: 0, refs: new Map(), ids: new WeakMap() });
    const INTERACTIVE =
        'a[href],button,input:not([type=hidden]),select,textarea,summary,[role=button],[role=link],[role=checkbox],[role=radio],[role=switch],[role=tab],[role=menuitem],[role=menuitemcheckbox],[role=menuitemradio],[role=option],[role=combobox],[role=textbox],[role=searchbox],[role=slider],[role=spinbutton],[role=treeitem],[contenteditable=""],[contenteditable=true],[onclick],[tabindex]:not([tabindex="-1"])';
    const SECTIONS = {
        FORM: 'form',
        DIALOG: 'dialog',
        NAV: 'nav',
        MAIN: 'main',
        HEADER: 'header',
        FOOTER: 'footer',
        ASIDE: 'aside',
        TABLE: 'table',
        TR: 'row',
        LI: 'item',
    };
    const ROLE_SECTIONS = [
        'dialog',
        'alertdialog',
        'navigation',
        'form',
        'main',
        'banner',
        'contentinfo',
        'complementary',
        'menu',
        'listbox',
        'tablist',
        'grid',
        'row',
        'region',
        'search',
    ];
    const SKIP = ['SCRIPT', 'STYLE', 'NOSCRIPT', 'TEMPLATE', 'HEAD', 'SVG'];
    const clip = (s, n = 100) =>
        String(s ?? '')
            .trim()
            .replace(/\s+/g, ' ')
            .slice(0, n);
    const visible = (el) => (el.checkVisibility ? el.checkVisibility({ checkOpacity: true, checkVisibilityCSS: true }) : el.getClientRects().length > 0);
    // A <label> that wraps its control would otherwise include the control's own text (a select's options).
    const labelText = (l) => {
        const c = l.cloneNode(true);
        c.querySelectorAll('select,input,textarea,button').forEach((x) => x.remove());
        return c.textContent;
    };
    const name = (el) => {
        const root = el.getRootNode();
        const by = el.getAttribute('aria-labelledby');
        const labelled =
            by &&
            by
                .split(/\s+/)
                .map((id) => root.getElementById?.(id)?.innerText ?? '')
                .join(' ');
        const button = el.tagName === 'INPUT' && ['submit', 'button', 'reset'].includes(el.type) ? el.value : '';
        return clip(
            labelled ||
                el.getAttribute('aria-label') ||
                [...(el.labels ?? [])].map(labelText).join(' ') ||
                button ||
                el.innerText ||
                el.getAttribute('alt') ||
                el.querySelector?.('img[alt]')?.alt ||
                el.title ||
                el.placeholder ||
                el.name,
        );
    };
    const kindOf = (el) =>
        el.getAttribute('role') ||
        (el.tagName === 'INPUT' ? el.type : el.tagName === 'A' ? 'link' : el.isContentEditable ? 'editor' : el.tagName.toLowerCase());
    const refFor = (el, kind, nm) => {
        let r = S.ids.get(el);
        if (!r) S.ids.set(el, (r = `e${++S.n}`));
        S.refs.set(r, { el: new WeakRef(el), kind, name: nm });
        return r;
    };

    function snap(limit) {
        const out = [];
        let truncated = false;
        const vh = innerHeight;
        const vw = innerWidth;
        const describe = (el, depth) => {
            const tag = el.tagName;
            const kind = kindOf(el);
            const nm = name(el);
            const e = { depth, ref: refFor(el, kind, nm), kind, name: nm };
            if (tag === 'A') e.href = el.href;
            if (
                ['INPUT', 'TEXTAREA', 'SELECT'].includes(tag) &&
                !['checkbox', 'radio', 'submit', 'button', 'reset', 'image', 'file'].includes(el.type) &&
                el.value
            ) {
                e.value = el.type === 'password' ? '••••' : clip(tag === 'SELECT' ? el.selectedOptions[0]?.text : el.value, 60);
            }
            if (tag === 'SELECT') e.options = [...el.options].slice(0, 20).map((o) => clip(o.text, 40));
            const aria = (k) => el.getAttribute(`aria-${k}`);
            if (el.checked || aria('checked') === 'true') e.checked = true;
            if (aria('selected') === 'true') e.selected = true;
            if (aria('expanded')) e.expanded = aria('expanded') === 'true';
            if (el.disabled || aria('disabled') === 'true') e.disabled = true;
            if (el.required) e.required = true;
            if (el.placeholder && el.placeholder !== nm) e.placeholder = clip(el.placeholder, 40);
            const r = el.getBoundingClientRect();
            if (r.bottom < 0 || r.top > vh || r.right < 0 || r.left > vw) e.offscreen = true;
            else {
                const hit = el.getRootNode().elementFromPoint?.(r.left + r.width / 2, r.top + r.height / 2);
                if (hit && !el.contains(hit) && !hit.contains(el)) e.covered = true;
            }
            return e;
        };
        const walk = (node, depth) => {
            for (const el of node.children) {
                if (out.length >= limit) {
                    truncated = true;
                    return;
                }
                if (SKIP.includes(el.tagName.toUpperCase()) || el.getAttribute('aria-hidden') === 'true') continue;
                const st = getComputedStyle(el);
                if (st.display !== 'contents' && !visible(el)) continue;
                const role = el.getAttribute('role');
                const pointer = st.cursor === 'pointer' && el.parentElement && getComputedStyle(el.parentElement).cursor !== 'pointer';
                if (el.matches(INTERACTIVE) || pointer) {
                    out.push(describe(el, depth));
                    continue;
                }
                if (/^H[1-4]$/.test(el.tagName) || role === 'heading') {
                    const t = clip(el.innerText, 80);
                    if (t) out.push({ depth, kind: 'heading', name: t });
                    continue;
                }
                const sec = SECTIONS[el.tagName] ?? (ROLE_SECTIONS.includes(role) ? role : null);
                if (sec) {
                    const at = out.length;
                    const listy = sec === 'row' || sec === 'item';
                    const label = listy ? clip(el.innerText, 60) : clip(el.getAttribute('aria-label') ?? (el.tagName === 'FORM' ? el.name : ''), 60);
                    out.push({ depth, kind: sec, name: label });
                    walk(el, depth + 1);
                    if (el.shadowRoot) walk(el.shadowRoot, depth + 1);
                    const kids = out.length - at - 1;
                    // Drop empty sections, and rows/items that are nothing but one element (a nav link).
                    if (kids === 0 || (listy && kids === 1 && label === out[at + 1].name)) {
                        out.splice(at, 1);
                        for (let i = at; i < out.length; i++) out[i].depth -= 1;
                    }
                    continue;
                }
                walk(el, depth);
                if (el.shadowRoot) walk(el.shadowRoot, depth);
            }
        };
        if (document.body) walk(document.body, 0);
        const se = document.scrollingElement ?? document.documentElement;
        return { url: location.href, title: document.title, scrollY: Math.round(scrollY), viewport: vh, height: se.scrollHeight, truncated, elements: out };
    }

    function get(ref) {
        const s = S.refs.get(ref);
        const el = s?.el.deref();
        if (el?.isConnected) return el;
        if (s) {
            // Re-rendered: find the one element that now looks the same.
            snap(5000);
            const same = [...S.refs.entries()].filter(([r, v]) => r !== ref && v.kind === s.kind && v.name === s.name && v.el.deref()?.isConnected);
            if (same.length === 1) {
                const fresh = same[0][1].el.deref();
                S.refs.set(ref, { ...s, el: new WeakRef(fresh) });
                return fresh;
            }
        }
        throw new Error(`no element ${ref} on the page any more: take a fresh browse_snapshot`);
    }

    const point = (el) => {
        el.scrollIntoView({ block: 'center', inline: 'center' });
        const r = el.getBoundingClientRect();
        return { bubbles: true, cancelable: true, composed: true, view: window, clientX: r.left + r.width / 2, clientY: r.top + r.height / 2 };
    };

    function click(el) {
        if (el.disabled || el.getAttribute('aria-disabled') === 'true') throw new Error('that element is disabled');
        const p = point(el);
        el.dispatchEvent(new PointerEvent('pointerdown', { ...p, pointerType: 'mouse', isPrimary: true }));
        el.dispatchEvent(new MouseEvent('mousedown', p));
        el.focus?.({ preventScroll: true });
        el.dispatchEvent(new PointerEvent('pointerup', { ...p, pointerType: 'mouse', isPrimary: true }));
        el.dispatchEvent(new MouseEvent('mouseup', p));
        el.click();
        return 'clicked';
    }

    function hover(el) {
        const p = point(el);
        for (const t of ['pointerover', 'pointerenter', 'pointermove']) el.dispatchEvent(new PointerEvent(t, { ...p, pointerType: 'mouse' }));
        for (const t of ['mouseover', 'mouseenter', 'mousemove']) el.dispatchEvent(new MouseEvent(t, p));
        return 'hovered';
    }

    function setValue(el, value) {
        const proto =
            el instanceof HTMLTextAreaElement
                ? HTMLTextAreaElement.prototype
                : el instanceof HTMLSelectElement
                  ? HTMLSelectElement.prototype
                  : HTMLInputElement.prototype;
        Object.getOwnPropertyDescriptor(proto, 'value').set.call(el, value);
        el.dispatchEvent(new Event('input', { bubbles: true, composed: true }));
        el.dispatchEvent(new Event('change', { bubbles: true, composed: true }));
    }

    function select(el, want) {
        if (el.tagName !== 'SELECT') throw new Error('not a <select>: click it to open the list, take a snapshot, then click the option');
        const opts = [...el.options];
        const w = String(want).trim().toLowerCase();
        const exact = opts.filter((o) => o.value === want || o.text.trim() === want);
        const loose = opts.filter((o) => o.text.trim().toLowerCase() === w);
        const partial = opts.filter((o) => o.text.toLowerCase().includes(w));
        const pick = exact[0] ?? loose[0] ?? (partial.length === 1 ? partial[0] : null);
        if (!pick) throw new Error(`no single option matches "${want}"; options: ${opts.map((o) => o.text.trim()).join(' | ')}`);
        el.focus();
        setValue(el, pick.value);
        return `selected "${pick.text.trim()}"`;
    }

    function type(el, text) {
        if (el.tagName === 'SELECT') return select(el, text);
        point(el);
        el.focus();
        if (el.isContentEditable) getSelection().selectAllChildren(el);
        else el.select?.();
        // insertText goes through the browser's editing path, which rich editors and React both see.
        const ok = text === '' ? document.execCommand('delete') : document.execCommand('insertText', false, text);
        if (el.isContentEditable) {
            if (!ok || el.innerText.trim() !== text.trim()) {
                el.textContent = text;
                el.dispatchEvent(new InputEvent('input', { bubbles: true, composed: true, inputType: 'insertText', data: text }));
            }
        } else if (!ok || el.value !== text) setValue(el, text);
        else el.dispatchEvent(new Event('change', { bubbles: true, composed: true }));
        return 'typed';
    }

    function key(target, combo) {
        const parts = combo.split('+');
        const k = parts.pop();
        const mods = { ctrlKey: parts.includes('Control'), shiftKey: parts.includes('Shift'), altKey: parts.includes('Alt'), metaKey: parts.includes('Meta') };
        const code = k.length === 1 ? (/[a-z]/i.test(k) ? `Key${k.toUpperCase()}` : /\d/.test(k) ? `Digit${k}` : k) : k;
        const opts = { key: k, code, bubbles: true, cancelable: true, composed: true, ...mods };
        const notCancelled = target.dispatchEvent(new KeyboardEvent('keydown', opts));
        if (k.length === 1 || k === 'Enter') target.dispatchEvent(new KeyboardEvent('keypress', opts));
        target.dispatchEvent(new KeyboardEvent('keyup', opts));
        if (!notCancelled) return `pressed ${combo} (the page handled it)`;
        if (k === 'Enter' && target.form && target.tagName === 'INPUT') target.form.requestSubmit();
        if (k === 'Tab') {
            const all = [...document.querySelectorAll('a[href],button,input,select,textarea,[tabindex]:not([tabindex="-1"]),[contenteditable=true]')].filter(
                (e) => !e.disabled && visible(e),
            );
            const i = all.indexOf(target);
            all[(i + (mods.shiftKey ? -1 : 1) + all.length) % all.length]?.focus();
        }
        return `pressed ${combo}`;
    }

    function scroll({ ref, direction, pages = 1, text }) {
        if (text) {
            const w = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT);
            const needle = text.toLowerCase();
            while (w.nextNode()) {
                const parent = w.currentNode.parentElement;
                if (parent && w.currentNode.textContent.toLowerCase().includes(needle) && visible(parent)) {
                    parent.scrollIntoView({ block: 'center' });
                    return `scrolled to "${text}"`;
                }
            }
            throw new Error(`"${text}" isn't on the page`);
        }
        if (ref && !direction) {
            get(ref).scrollIntoView({ block: 'center' });
            return `scrolled ${ref} into view`;
        }
        const box = ref ? get(ref) : (document.scrollingElement ?? document.documentElement);
        const step = (ref ? box.clientHeight : innerHeight) * 0.9 * pages;
        box.scrollBy({ top: direction === 'up' ? -step : step, behavior: 'instant' });
        return `scrolled ${direction ?? 'down'}`;
    }

    function settle(quietMs = 200, capMs = 3000) {
        return new Promise((resolve) => {
            let timer;
            const finish = () => {
                obs.disconnect();
                clearTimeout(cap);
                resolve();
            };
            const obs = new MutationObserver(() => {
                clearTimeout(timer);
                timer = setTimeout(finish, quietMs);
            });
            obs.observe(document, { subtree: true, childList: true, attributes: true, characterData: true });
            timer = setTimeout(finish, quietMs);
            const cap = setTimeout(finish, capMs);
        });
    }

    async function waitFor(texts, timeoutMs) {
        const end = Date.now() + timeoutMs;
        for (;;) {
            const body = document.body?.innerText ?? '';
            const hit = texts.find((t) => body.includes(t));
            if (hit !== undefined) return hit;
            if (Date.now() > end) throw new Error(`none of ${JSON.stringify(texts)} appeared within ${timeoutMs}ms`);
            await new Promise((r) => setTimeout(r, 250));
        }
    }

    switch (op) {
        case 'snapshot':
            return snap(a.limit ?? 400);
        case 'text':
            return { url: location.href, title: document.title, text: document.body?.innerText ?? '' };
        case 'click':
            return click(get(a.ref));
        case 'hover':
            return hover(get(a.ref));
        case 'type': {
            const el = get(a.ref);
            const r = type(el, a.text);
            if (a.submit) {
                if (el.form) el.form.requestSubmit();
                else key(el, 'Enter');
            }
            return r;
        }
        case 'select':
            return select(get(a.ref), a.option);
        case 'key':
            return key(a.ref ? get(a.ref) : (document.activeElement ?? document.body), a.key);
        case 'scroll':
            return scroll(a);
        case 'fill':
            return a.fields.map(({ ref, value }) => {
                const el = get(ref);
                if (el.type === 'checkbox' || el.type === 'radio' || ['checkbox', 'radio', 'switch'].includes(el.getAttribute('role'))) {
                    const want = !['false', '0', 'off', 'no', ''].includes(String(value).toLowerCase());
                    const now = el.checked ?? el.getAttribute('aria-checked') === 'true';
                    if (now !== want) click(el);
                    return `${ref}: ${want ? 'checked' : 'unchecked'}`;
                }
                return `${ref}: ${type(el, String(value))}`;
            });
        case 'center': {
            // Where a trusted click lands, and what is on top there.
            const el = get(a.ref);
            if (el.disabled || el.getAttribute('aria-disabled') === 'true') throw new Error('that element is disabled');
            const p = point(el);
            const root = el.getRootNode();
            const top = (root.elementFromPoint ? root : document).elementFromPoint(p.clientX, p.clientY);
            const covered = top && top !== el && !el.contains(top) && !top.contains(el);
            return { x: p.clientX, y: p.clientY, covered: covered ? `<${top.tagName.toLowerCase()}${top.id ? `#${top.id}` : ''}>` : undefined };
        }
        case 'focus':
        case 'focusAll': {
            const el = get(a.ref);
            point(el);
            el.focus();
            if (op === 'focusAll') {
                if (el.isContentEditable) getSelection().selectAllChildren(el);
                else el.select?.();
            }
            return 'focused';
        }
        case 'mark': {
            // Tag the file input so the debugger can find it with DOM.querySelector.
            let el;
            if (a.ref) el = get(a.ref);
            else {
                const all = document.querySelectorAll('input[type=file]');
                if (all.length !== 1) throw new Error(`the page has ${all.length} file inputs: pass the ref of the one to use`);
                el = all[0];
            }
            if (!(el instanceof HTMLInputElement && el.type === 'file')) throw new Error('that is not a file input (<input type=file>)');
            el.setAttribute('data-smooth-upload', a.mark);
            return el.labels?.[0]?.innerText?.trim() || el.name || 'the file input';
        }
        case 'unmark':
            document.querySelector(`[data-smooth-upload="${a.mark}"]`)?.removeAttribute('data-smooth-upload');
            return 'unmarked';
        case 'settle':
            return settle();
        case 'waitFor':
            return waitFor(a.texts, a.timeoutMs);
        default:
            throw new Error(`unknown page op ${op}`);
    }
}

const METHODS = {
    'bookmarks.getTree': () => chrome.bookmarks.getTree(),
    'bookmarks.getSubTree': ({ id }) => chrome.bookmarks.getSubTree(id),
    'bookmarks.getChildren': ({ id }) => chrome.bookmarks.getChildren(id),
    'bookmarks.get': ({ ids }) => chrome.bookmarks.get(ids),
    'bookmarks.search': ({ query }) => chrome.bookmarks.search(query),
    'bookmarks.create': ({ parentId, title, url, index }) => chrome.bookmarks.create(clean({ parentId, title, url, index })),
    'bookmarks.move': ({ id, parentId, index }) => chrome.bookmarks.move(id, clean({ parentId, index })),
    'bookmarks.update': ({ id, title, url }) => chrome.bookmarks.update(id, clean({ title, url })),
    'bookmarks.remove': ({ id }) => chrome.bookmarks.remove(id),
    'bookmarks.removeTree': ({ id }) => chrome.bookmarks.removeTree(id),

    'browse.status': async () => ({ enabled: await browsingEnabled() }),
    'tabs.list': async () => {
        await needBrowsing();
        return (await chrome.tabs.query({})).map(tabInfo);
    },
    'tabs.open': async ({ url, active = true }) => {
        await needBrowsing();
        const t = await chrome.tabs.create({ url, active });
        await groupTab(t);
        return tabInfo(await waitForLoad(t.id));
    },
    'tabs.navigate': async ({ tabId, url }) => {
        await needBrowsing();
        await chrome.tabs.update(tabId, { url });
        await sleep(100);
        return tabInfo(await waitForLoad(tabId));
    },
    'tabs.history': async ({ tabId, action }) => {
        await needBrowsing();
        const go = { back: (id) => chrome.tabs.goBack(id), forward: (id) => chrome.tabs.goForward(id), reload: (id) => chrome.tabs.reload(id) }[action];
        if (!go) throw new Error('action must be back, forward or reload');
        await go(tabId);
        await sleep(150);
        return tabInfo(await waitForLoad(tabId));
    },
    'tabs.activate': async ({ tabId }) => {
        await needBrowsing();
        const t = await chrome.tabs.update(tabId, { active: true });
        await chrome.windows.update(t.windowId, { focused: true });
        return tabInfo(t);
    },
    'tabs.close': async ({ tabId }) => {
        await needBrowsing();
        await chrome.tabs.remove(tabId);
    },
    'page.text': ({ tabId }) => inPage(tabId, 'text'),
    'page.snapshot': ({ tabId, limit }) => snapshot(tabId, limit),
    'page.act': ({ tabId, op, args }) => act(tabId, op, args ?? {}),
    'page.waitFor': ({ tabId, texts, timeoutMs = 10000 }) => inPage(tabId, 'waitFor', { texts, timeoutMs }, 0, timeoutMs + 5000),
    'page.screenshot': async ({ tabId }) => {
        await needBrowsing();
        if (await trustedEnabled()) {
            // Through the debugger: no tab switch, and scaled to CSS pixels so a point in the
            // image is the x/y to click.
            const { metrics, width, height } = await viewport(tabId);
            const dpr = (await cdp(tabId, 'Runtime.evaluate', { expression: 'devicePixelRatio', returnByValue: true })).result.value;
            const params = SmoothCdp.viewportClip(metrics, dpr);
            // A background tab may not have painted; a throwaway capture forces a frame.
            if (!(await chrome.tabs.get(tabId)).active) await cdp(tabId, 'Page.captureScreenshot', { ...params, format: 'jpeg', quality: 1 }).catch(() => {});
            const { data } = await cdp(tabId, 'Page.captureScreenshot', params);
            return { dataUrl: `data:image/png;base64,${data}`, width, height, cssPixels: true };
        }
        const t = await chrome.tabs.update(tabId, { active: true });
        return { dataUrl: await chrome.tabs.captureVisibleTab(t.windowId, { format: 'png' }) };
    },
    'trusted.status': async () => ({ enabled: await trustedEnabled() }),
    'page.eval': async ({ tabId, code, timeoutMs = 10000 }) => {
        const run = (expression) =>
            cdp(tabId, 'Runtime.evaluate', { expression, replMode: true, returnByValue: true, awaitPromise: true, userGesture: true, timeout: timeoutMs });
        let r = await run(code);
        const why = (d) => d.exception?.description ?? d.text;
        // A bare `return` is fine in a function body: retry wrapped in one.
        if (r.exceptionDetails && /Illegal return/.test(why(r.exceptionDetails))) r = await run(`(async () => {\n${code}\n})()`);
        if (r.exceptionDetails) throw new Error(why(r.exceptionDetails));
        return clean({ type: r.result.type, value: r.result.value, unserializable: r.result.unserializableValue, description: r.result.description });
    },
    'page.console': async ({ tabId, clear, stop }) => {
        const s = await session(tabId);
        const started = !s.console && !stop;
        if (started) {
            s.console = [];
            await cdp(tabId, 'Runtime.enable');
            await sleep(100); // Runtime.enable replays what the page already logged.
        }
        const messages = s.console ? [...s.console] : [];
        if (clear && s.console) s.console.length = 0;
        if (stop && s.console) {
            s.console = null;
            await cdp(tabId, 'Runtime.disable').catch(() => {});
        }
        return { started, messages };
    },
    'page.network': async ({ tabId, clear, stop }) => {
        const s = await session(tabId);
        const started = !s.network && !stop;
        if (started) {
            s.network = new Map();
            await cdp(tabId, 'Network.enable', { maxPostDataSize: 0 });
        }
        const requests = s.network ? [...s.network.values()] : [];
        if (clear && s.network) s.network.clear();
        if (stop && s.network) {
            s.network = null;
            await cdp(tabId, 'Network.disable').catch(() => {});
        }
        return { started, requests };
    },
    'page.dialog': async ({ tabId, action = 'show', text }) => {
        const s = await session(tabId);
        const d = s.dialog;
        if (!d || action === 'show') return { open: !!d, ...d };
        if (action !== 'accept' && action !== 'dismiss') throw new Error('action must be show, accept or dismiss');
        await cdp(tabId, 'Page.handleJavaScriptDialog', clean({ accept: action === 'accept', promptText: text }));
        s.dialog = null;
        return { open: true, handled: action, ...d };
    },
};

chrome.runtime.onMessage.addListener((msg, _sender, reply) => {
    if (msg?.type !== 'status') return;
    connect();
    browsingEnabled().then((browsing) => reply({ connected: !!port, lastError, browsing }));
    return true;
});
// Turning trusted input off (or revoking browsing, which it needs) ends every debugger session.
async function trustedChanged() {
    if (!(await trustedEnabled())) {
        await Promise.all([...dbg.keys()].map((tabId) => chrome.debugger?.detach({ tabId }).catch(() => {})));
        for (const s of dbg.values()) clearTimeout(s.idle);
        dbg.clear();
    }
    hello();
}
chrome.permissions.onAdded.addListener(() => {
    listenDebugger();
    hello();
});
chrome.permissions.onRemoved.addListener(trustedChanged);
chrome.storage.onChanged.addListener((changes, area) => {
    if (area === 'local' && TRUSTED_KEY in changes) trustedChanged();
});
chrome.runtime.onStartup.addListener(connect);
chrome.runtime.onInstalled.addListener(connect);
chrome.alarms.onAlarm.addListener(connect);
chrome.alarms.create('reconnect', { periodInMinutes: 1 });
connect();

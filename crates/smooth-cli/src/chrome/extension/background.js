const HOST = 'ai.smoo.smooth_chrome';
const BROWSING = { permissions: ['tabs', 'scripting'], origins: ['<all_urls>'] };

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

function retry() {
    setTimeout(connect, backoff);
    backoff = Math.min(backoff * 2, 60000);
}

async function hello() {
    port?.postMessage({ type: 'hello', version: chrome.runtime.getManifest().version, browsing: await browsingEnabled() });
}

const browsingEnabled = () => chrome.permissions.contains(BROWSING);

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

// Chrome's schema validation rejects null for optional fields; drop them.
const clean = (o) => Object.fromEntries(Object.entries(o).filter(([, v]) => v !== null && v !== undefined));

const tabInfo = (t) => t && { tabId: t.id, windowId: t.windowId, url: t.url, title: t.title, active: t.active, status: t.status };

async function inPage(tabId, func, args = []) {
    await needBrowsing();
    const [res] = await chrome.scripting.executeScript({ target: { tabId }, func, args });
    return res?.result;
}

function pageSnapshot(limit) {
    const sel = 'a[href],button,input,select,textarea,summary,[role=button],[role=link],[role=checkbox],[role=tab],[role=menuitem],[contenteditable=true]';
    const out = [];
    let n = 0;
    for (const el of document.querySelectorAll(sel)) {
        const r = el.getBoundingClientRect();
        const style = getComputedStyle(el);
        if (r.width === 0 || r.height === 0 || style.visibility === 'hidden' || style.display === 'none') continue;
        const ref = el.dataset.smoothRef || `e${++n}`;
        el.dataset.smoothRef = ref;
        const name = (el.getAttribute('aria-label') || el.innerText || el.value || el.placeholder || el.title || el.name || '')
            .trim()
            .replace(/\s+/g, ' ')
            .slice(0, 120);
        out.push({
            ref,
            tag: el.tagName.toLowerCase(),
            role: el.getAttribute('role') || undefined,
            type: el.type || undefined,
            name,
            href: el.href || undefined,
        });
        if (out.length >= limit) break;
    }
    return { url: location.href, title: document.title, elements: out };
}

function pageAct(ref, action, text, submit) {
    const el = document.querySelector(`[data-smooth-ref="${CSS.escape(ref)}"]`);
    if (!el) throw new Error(`no element ${ref}: take a fresh snapshot`);
    el.scrollIntoView({ block: 'center' });
    if (action === 'click') {
        el.click();
        return { ok: true };
    }
    el.focus();
    if (el.isContentEditable) el.textContent = text;
    else {
        const proto =
            el instanceof HTMLTextAreaElement
                ? HTMLTextAreaElement.prototype
                : el instanceof HTMLSelectElement
                  ? HTMLSelectElement.prototype
                  : HTMLInputElement.prototype;
        Object.getOwnPropertyDescriptor(proto, 'value').set.call(el, text);
    }
    el.dispatchEvent(new Event('input', { bubbles: true }));
    el.dispatchEvent(new Event('change', { bubbles: true }));
    if (submit) {
        if (el.form) el.form.requestSubmit();
        else
            for (const type of ['keydown', 'keypress', 'keyup'])
                el.dispatchEvent(new KeyboardEvent(type, { key: 'Enter', code: 'Enter', keyCode: 13, bubbles: true }));
    }
    return { ok: true };
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
        return tabInfo(await waitForLoad(t.id));
    },
    'tabs.navigate': async ({ tabId, url }) => {
        await needBrowsing();
        await chrome.tabs.update(tabId, { url });
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
    'page.text': ({ tabId }) => inPage(tabId, () => ({ url: location.href, title: document.title, text: document.body?.innerText ?? '' })),
    'page.snapshot': ({ tabId, limit = 300 }) => inPage(tabId, pageSnapshot, [limit]),
    'page.click': ({ tabId, ref }) => inPage(tabId, pageAct, [ref, 'click', '', false]),
    'page.type': ({ tabId, ref, text, submit = false }) => inPage(tabId, pageAct, [ref, 'type', text, submit]),
    'page.screenshot': async ({ tabId }) => {
        await needBrowsing();
        const t = await chrome.tabs.update(tabId, { active: true });
        return { dataUrl: await chrome.tabs.captureVisibleTab(t.windowId, { format: 'png' }) };
    },
};

chrome.runtime.onMessage.addListener((msg, _sender, reply) => {
    if (msg?.type !== 'status') return;
    browsingEnabled().then((browsing) => reply({ connected: !!port, lastError, browsing }));
    return true;
});
chrome.permissions.onAdded.addListener(hello);
chrome.permissions.onRemoved.addListener(hello);
chrome.runtime.onStartup.addListener(connect);
chrome.runtime.onInstalled.addListener(connect);
connect();

const HOST = 'ai.smoo.smooth_chrome';
const BROWSING = { permissions: ['tabs', 'scripting', 'tabGroups'], origins: ['<all_urls>'] };

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
        throw new Error('the page is not responding: a JavaScript dialog (alert/confirm) may be open; ask the user to dismiss it');
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
            result = await inPage(tabId, op, { ...args, ref }, frameId);
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
        const t = await chrome.tabs.update(tabId, { active: true });
        return { dataUrl: await chrome.tabs.captureVisibleTab(t.windowId, { format: 'png' }) };
    },
};

chrome.runtime.onMessage.addListener((msg, _sender, reply) => {
    if (msg?.type !== 'status') return;
    connect();
    browsingEnabled().then((browsing) => reply({ connected: !!port, lastError, browsing }));
    return true;
});
chrome.permissions.onAdded.addListener(hello);
chrome.permissions.onRemoved.addListener(hello);
chrome.runtime.onStartup.addListener(connect);
chrome.runtime.onInstalled.addListener(connect);
chrome.alarms.onAlarm.addListener(connect);
chrome.alarms.create('reconnect', { periodInMinutes: 1 });
connect();

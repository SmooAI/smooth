// Replays the trusted-input tier's CDP builders (extension/cdp.js) against a fixture page in
// headless Chrome over --remote-debugging-port, and checks the page sees real input: isTrusted
// clicks (left, right, double), hover, keys, typed text, Enter submitting, select-all, wheel
// scroll, a screenshot scaled to CSS pixels, and browse_eval's return handling. Usage: node scripts/chrome-cdp-smoke.mjs
// (CHROME=/path overrides)
import { spawn } from 'node:child_process';
import { existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import vm from 'node:vm';

const cdpSrc = readFileSync(new URL('../crates/smooth-cli/src/chrome/extension/cdp.js', import.meta.url), 'utf8');
const ctx = vm.createContext({});
vm.runInContext(`${cdpSrc}\nglobalThis.SmoothCdp = SmoothCdp;`, ctx);
const C = ctx.SmoothCdp;
// The extension passes mac by the browser's OS; match it so Mac editing commands are exercised.
const mac = process.platform === 'darwin';

const html = `<!doctype html><html><head><title>CDP fixture</title>
<style>body{margin:0;font:16px sans-serif} #btn,#hov{position:absolute;left:20px;width:120px;height:40px} #btn{top:20px} #hov{top:80px}
#f{position:absolute;top:140px;left:20px} #t{width:300px;height:30px}</style></head><body>
<button id="btn">Press</button><div id="hov">Hover me</div>
<form id="f"><input id="t" autocomplete="off"></form>
<div style="height:4000px"></div>
<script>
window.log = [];
const rec = (e) => log.push({ type: e.type, trusted: e.isTrusted, button: e.button, detail: e.detail, key: e.key });
for (const t of ['click', 'contextmenu', 'dblclick']) document.getElementById('btn').addEventListener(t, (e) => { e.preventDefault(); rec(e); });
document.getElementById('hov').addEventListener('mouseover', rec);
const input = document.getElementById('t');
for (const t of ['keydown', 'input', 'beforeinput']) input.addEventListener(t, rec);
document.getElementById('f').addEventListener('submit', (e) => { e.preventDefault(); window.submitted = true; });
</script></body></html>`;

const chrome =
    process.env.CHROME ??
    ['/Applications/Google Chrome.app/Contents/MacOS/Google Chrome', '/usr/bin/google-chrome', '/usr/bin/chromium', '/usr/bin/chromium-browser'].find(
        existsSync,
    );
if (!chrome) {
    console.log('skip: no Chrome found (set CHROME)');
    process.exit(0);
}
if (typeof WebSocket !== 'function') {
    console.log('skip: this Node has no global WebSocket (needs Node 22+)');
    process.exit(0);
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const dir = mkdtempSync(join(tmpdir(), 'smooth-cdp-'));
writeFileSync(join(dir, 'fixture.html'), html);
const profile = join(dir, 'profile');
const proc = spawn(
    chrome,
    [
        '--headless=new',
        '--disable-gpu',
        '--no-first-run',
        '--remote-debugging-port=0',
        `--user-data-dir=${profile}`,
        '--window-size=1000,700',
        // A 2x screen, so the screenshot check proves the image is scaled back to CSS pixels.
        '--force-device-scale-factor=2',
        `file://${join(dir, 'fixture.html')}`,
    ],
    { stdio: 'ignore' },
);

let ws;
const R = {};
try {
    let port;
    for (let i = 0; i < 100 && !port; i++) {
        await sleep(100);
        const f = join(profile, 'DevToolsActivePort');
        if (existsSync(f)) port = readFileSync(f, 'utf8').split('\n')[0];
    }
    if (!port) throw new Error('Chrome never opened a debugging port');
    let page;
    for (let i = 0; i < 50 && !page; i++) {
        page = (await (await fetch(`http://127.0.0.1:${port}/json/list`)).json()).find((t) => t.type === 'page' && t.url.startsWith('file:'));
        if (!page) await sleep(100);
    }
    if (!page) throw new Error('fixture tab not found');

    ws = new WebSocket(page.webSocketDebuggerUrl);
    await new Promise((ok, fail) => {
        ws.onopen = ok;
        ws.onerror = fail;
    });
    let nextId = 0;
    const pending = new Map();
    ws.onmessage = (m) => {
        const msg = JSON.parse(m.data);
        const p = pending.get(msg.id);
        if (!p) return;
        pending.delete(msg.id);
        if (msg.error) p.fail(new Error(`${p.method}: ${msg.error.message}`));
        else p.ok(msg.result);
    };
    const cdp = (method, params = {}) =>
        new Promise((ok, fail) => {
            const id = ++nextId;
            pending.set(id, { ok, fail, method });
            ws.send(JSON.stringify({ id, method, params }));
        });
    const evaluate = async (expression) => (await cdp('Runtime.evaluate', { expression, returnByValue: true, awaitPromise: true })).result.value;
    const run = async (events, method) => {
        for (const e of events) await cdp(method, e);
    };
    const mouse = (events) => run(events, 'Input.dispatchMouseEvent');
    const keys = (combo) => run(C.keyEvents(combo, { mac }), 'Input.dispatchKeyEvent');
    const type = async (text) => {
        for (const [method, params] of C.typeCalls(text, { mac })) await cdp(method, params);
    };
    const center = (sel) =>
        evaluate(
            `(() => { const r = document.querySelector(${JSON.stringify(sel)}).getBoundingClientRect(); return { x: r.x + r.width / 2, y: r.y + r.height / 2 }; })()`,
        );
    const takeLog = () => evaluate('window.log.splice(0)');

    await cdp('Page.enable');
    for (let i = 0; i < 50 && (await evaluate('document.readyState')) !== 'complete'; i++) await sleep(100);

    // browse_eval, exactly as the extension runs it.
    const ev = async (code) => (await C.evaluate((params) => cdp('Runtime.evaluate', params), code, 5000)).value;
    R.evalExpr = await ev('const x = 5; x * 2');
    R.evalAwait = await ev('await new Promise((r) => setTimeout(r, 10)); 7');
    R.evalReturn = await ev('return 42');
    R.evalAsyncReturn = await ev("await new Promise((r) => setTimeout(r, 10)); return 'async ok'");
    R.evalThrows = await ev("throw new Error('boom')").catch((e) => e.message);

    const b = await center('#btn');
    await mouse(C.clickEvents(b.x, b.y));
    R.click = await takeLog();
    await mouse(C.clickEvents(b.x, b.y, { button: 'right' }));
    R.right = await takeLog();
    await mouse(C.clickEvents(b.x, b.y, { count: 2 }));
    R.double = await takeLog();

    const h = await center('#hov');
    await mouse(C.hoverEvents(h.x, h.y));
    R.hover = await takeLog();

    const t = await center('#t');
    await mouse(C.clickEvents(t.x, t.y));
    await takeLog();
    await type('héllo wörld');
    R.typed = await evaluate('document.getElementById("t").value');
    R.typeLog = await takeLog();
    await keys('Backspace');
    R.afterBackspace = await evaluate('document.getElementById("t").value');
    R.keyLog = await takeLog();
    await keys(mac ? 'Meta+a' : 'Control+a');
    await type('new');
    R.afterSelectAll = await evaluate('document.getElementById("t").value');
    await keys('Enter');
    R.submitted = await evaluate('window.submitted === true');

    await mouse(C.wheelEvents(500, 400, 0, 600));
    for (let i = 0; i < 20 && !((await evaluate('scrollY')) > 0); i++) await sleep(50);
    R.scrollY = await evaluate('scrollY');
    await evaluate('scrollTo(0, 0)');

    const metrics = await cdp('Page.getLayoutMetrics');
    const dpr = await evaluate('devicePixelRatio');
    const { data } = await cdp('Page.captureScreenshot', C.viewportClip(metrics, dpr));
    const png = Buffer.from(data, 'base64');
    R.dpr = dpr;
    R.shot = { width: png.readUInt32BE(16), height: png.readUInt32BE(20) };
    R.viewport = await evaluate('({ width: innerWidth, height: innerHeight })');

    try {
        C.checkPoint(R.viewport.width + 50, 10, R.viewport.width, R.viewport.height);
    } catch (e) {
        R.offscreenErr = e.message;
    }
} catch (e) {
    R.error = String(e.stack ?? e);
} finally {
    ws?.close();
    proc.kill();
    await sleep(200);
    rmSync(dir, { recursive: true, force: true });
}

const all = (log, type) => log?.filter((e) => e.type === type) ?? [];
const checks = {
    'no error': !R.error,
    'left click is trusted': all(R.click, 'click').length === 1 && all(R.click, 'click')[0].trusted,
    'right click fires contextmenu': all(R.right, 'contextmenu').some((e) => e.trusted && e.button === 2) && all(R.right, 'click').length === 0,
    'count 2 is a double click': all(R.double, 'dblclick').some((e) => e.trusted) && all(R.double, 'click').length === 2,
    'hover is trusted': all(R.hover, 'mouseover').some((e) => e.trusted),
    'typed text lands, unicode included': R.typed === 'héllo wörld',
    'typing fires trusted input': all(R.typeLog, 'input').length >= 1 && all(R.typeLog, 'input').every((e) => e.trusted),
    'Backspace deletes': R.afterBackspace === 'héllo wörl',
    'Backspace keydown is trusted': all(R.keyLog, 'keydown').some((e) => e.trusted && e.key === 'Backspace'),
    'select-all then type replaces': R.afterSelectAll === 'new',
    'Enter submits the form': R.submitted === true,
    'wheel scrolls': R.scrollY > 0,
    'screenshot is in CSS pixels': R.dpr === 2 && R.shot?.width === R.viewport?.width && R.shot?.height === R.viewport?.height,
    'off-viewport point refused': R.offscreenErr?.includes('outside'),
    'eval: last expression is the result': R.evalExpr === 10,
    'eval: top-level await': R.evalAwait === 7,
    'eval: bare return': R.evalReturn === 42,
    'eval: await then return': R.evalAsyncReturn === 'async ok',
    'eval: a throw is an error': R.evalThrows?.includes('boom'),
};
const failed = Object.entries(checks).filter(([, ok]) => !ok);
for (const [name, ok] of Object.entries(checks)) console.log(`${ok ? 'ok  ' : 'FAIL'} ${name}`);
if (failed.length) {
    console.error(JSON.stringify(R, null, 1));
    process.exit(1);
}

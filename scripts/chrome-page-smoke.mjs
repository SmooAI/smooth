// Runs the Smooth extension's in-page code (pageCall in background.js) against a fixture page in
// headless Chrome and checks every op: snapshot, type, select, fill, click (shadow DOM, re-rendered
// element), key, scroll, waitFor. Usage: node scripts/chrome-page-smoke.mjs  (CHROME=/path overrides)
import { execFileSync } from 'node:child_process';
import { existsSync, mkdtempSync, readFileSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

const bg = new URL('../crates/smooth-cli/src/chrome/extension/background.js', import.meta.url);
const src = readFileSync(bg, 'utf8');
const pageCall = src.slice(src.indexOf('async function pageCall'), src.indexOf('const METHODS = {'));

const html = `<!doctype html><html><head><title>Fixture</title></head><body>
<h1>Account</h1>
<form id="f" aria-label="Login">
  <label>Email <input id="email" type="email" required></label>
  <label for="pw">Password</label><input id="pw" type="password" value="secret">
  <label>Region <select id="region"><option value="us">United States</option><option value="eu">Europe</option></select></label>
  <label><input id="remember" type="checkbox"> Remember me</label>
  <textarea aria-label="Notes"></textarea>
  <button id="save" type="button">Save</button>
</form>
<div contenteditable="true" aria-label="Editor" id="editor"></div>
<ul><li>Order 1 <button>Edit</button></li><li>Order 2 <button>Edit</button></li><li><a href="#home">Home</a></li></ul>
<button style="display:none">Hidden</button>
<div aria-hidden="true"><button>AriaHidden</button></div>
<div class="card" style="cursor:pointer" onclickx="1">Clickable card</div>
<x-widget></x-widget>
<button id="behind" style="position:absolute;top:400px;left:10px">Behind</button>
<div style="position:fixed;top:380px;left:0;width:300px;height:80px;background:#0003"></div>
<div style="height:3000px"></div>
<a href="#far">Far link</a>
<pre id="out"></pre>
<script>
customElements.define('x-widget', class extends HTMLElement {
  constructor() { super(); this.attachShadow({ mode: 'open' }).innerHTML = '<button>Shadow button</button>';
    this.shadowRoot.querySelector('button').onclick = () => (window.shadowClicked = true); }
});
window.inputEvents = 0;
document.querySelector('#email').addEventListener('input', () => window.inputEvents++);
document.querySelector('#f').addEventListener('submit', (e) => { e.preventDefault(); window.submitted = true; });
document.querySelector('#save').onclick = () => (window.saveClickedOld = true);
setTimeout(() => document.body.insertAdjacentHTML('beforeend', '<p>Later text</p>'), 400);
${pageCall}
const R = {};
(async () => {
  try {
    const s1 = await pageCall('snapshot', { limit: 400 });
    R.snap1 = s1.elements.map((e) => JSON.stringify(e)).join('\\n');
    R.view = [s1.scrollY, s1.viewport, s1.height, s1.truncated];
    const ref = (n) => s1.elements.find((e) => e.name === n && e.ref)?.ref;
    R.type = await pageCall('type', { ref: ref('Email'), text: 'a@b.c' });
    R.emailValue = document.querySelector('#email').value;
    R.inputEvents = window.inputEvents;
    R.select = await pageCall('select', { ref: ref('Region'), option: 'europe' });
    R.regionValue = document.querySelector('#region').value;
    try { await pageCall('select', { ref: ref('Region'), option: 'mars' }); } catch (e) { R.selectErr = e.message; }
    R.fill = await pageCall('fill', { fields: [{ ref: ref('Remember me'), value: 'true' }, { ref: ref('Notes'), value: 'hello' }] });
    R.checked = document.querySelector('#remember').checked;
    R.notes = document.querySelector('textarea').value;
    R.editor = await pageCall('type', { ref: ref('Editor'), text: 'rich text' });
    R.editorText = document.querySelector('#editor').innerText;
    R.shadow = await pageCall('click', { ref: ref('Shadow button') });
    R.shadowClicked = window.shadowClicked;
    const saveRef = ref('Save');
    document.querySelector('#save').outerHTML = '<button id="save" type="button">Save</button>';
    document.querySelector('#save').onclick = () => (window.saveClicked = true);
    R.reresolve = await pageCall('click', { ref: saveRef });
    R.saveClicked = window.saveClicked;
    const s2 = await pageCall('snapshot', { limit: 400 });
    const refs = s2.elements.filter((e) => e.ref).map((e) => e.ref);
    R.refsUnique = new Set(refs).size === refs.length;
    R.emailRefStable = s2.elements.find((e) => e.name === 'Email')?.ref === ref('Email');
    R.key = await pageCall('key', { ref: ref('Email'), key: 'Enter' });
    R.submitted = window.submitted;
    R.scroll = await pageCall('scroll', { text: 'Far link' });
    R.scrollY = Math.round(scrollY) > 1000;
    R.wait = await pageCall('waitFor', { texts: ['Never', 'Later text'], timeoutMs: 3000 });
    await pageCall('settle', {});
    try { await pageCall('click', { ref: 'e9999' }); } catch (e) { R.staleErr = e.message; }
    R.text = (await pageCall('text', {})).text.length > 0;
  } catch (e) { R.error = String(e.stack); }
  document.getElementById('out').textContent = 'RESULT' + JSON.stringify(R, null, 1);
})();
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
const dir = mkdtempSync(join(tmpdir(), 'smooth-page-'));
writeFileSync(join(dir, 'fixture.html'), html);
const dom = execFileSync(
    chrome,
    [
        '--headless=new',
        '--disable-gpu',
        `--user-data-dir=${join(dir, 'profile')}`,
        '--window-size=1200,800',
        '--virtual-time-budget=8000',
        '--dump-dom',
        `file://${join(dir, 'fixture.html')}`,
    ],
    { encoding: 'utf8', stdio: ['ignore', 'pipe', 'ignore'] },
);
const raw = /RESULT([\s\S]*?)<\/pre>/.exec(dom)?.[1];
if (!raw) throw new Error('fixture produced no result');
const R = JSON.parse(raw.replaceAll('&quot;', '"').replaceAll('&lt;', '<').replaceAll('&gt;', '>').replaceAll('&amp;', '&'));
const snap = R.snap1.split('\n').map((l) => JSON.parse(l));
const by = (n) => snap.find((e) => e.name === n);
const checks = {
    'no error': !R.error,
    'form section with label': snap.some((e) => e.kind === 'form' && e.name === 'Login'),
    'select named by its label only': by('Region')?.options?.length === 2,
    'password value masked': by('Password')?.value === '••••',
    'rows keep their context': snap.some((e) => e.kind === 'item' && e.name === 'Order 1 Edit'),
    'single-link item collapsed': !snap.some((e) => e.kind === 'item' && e.name === 'Home'),
    'hidden + aria-hidden skipped': !by('Hidden') && !by('AriaHidden'),
    'cursor:pointer card found': !!by('Clickable card'),
    'shadow DOM button found': !!by('Shadow button'),
    'covered button flagged': by('Behind')?.covered === true,
    'offscreen link flagged': by('Far link')?.offscreen === true,
    'type sets value with input event': R.emailValue === 'a@b.c' && R.inputEvents >= 1,
    'select by loose text': R.regionValue === 'eu',
    'select lists options on miss': R.selectErr?.includes('United States | Europe'),
    'fill checkbox + textarea': R.checked === true && R.notes === 'hello',
    'rich editor typed': R.editorText === 'rich text',
    'shadow click': R.shadowClicked === true,
    're-rendered element re-found': R.saveClicked === true,
    'refs unique and stable': R.refsUnique && R.emailRefStable,
    'Enter submits': R.submitted === true,
    'scroll to text': R.scrollY === true,
    'waitFor resolves': R.wait === 'Later text',
    'stale ref explains': R.staleErr?.includes('fresh browse_snapshot'),
};
const failed = Object.entries(checks).filter(([, ok]) => !ok);
for (const [name, ok] of Object.entries(checks)) console.log(`${ok ? 'ok  ' : 'FAIL'} ${name}`);
if (failed.length) {
    console.error(JSON.stringify(R, null, 1));
    process.exit(1);
}

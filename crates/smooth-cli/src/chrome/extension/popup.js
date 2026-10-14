const BROWSING = { permissions: ['tabs', 'scripting', 'tabGroups'], origins: ['<all_urls>'] };
const TRUSTED = { permissions: ['debugger'] };
const $ = (id) => document.getElementById(id);

function setPill(state, text) {
    $('pill').className = `pill ${state}`;
    $('pill').querySelector('.glyph').textContent = { on: '●', off: '○', checking: '◐' }[state];
    $('conn').textContent = text;
}

function problem(html) {
    $('problem').hidden = !html;
    $('problem').innerHTML = html ?? '';
}

const escape = (s) => String(s).replace(/[&<>"]/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' })[c]);

async function toggle(set, on) {
    try {
        if (on) await chrome.permissions.request(set);
        else await chrome.permissions.remove(set);
    } finally {
        render();
    }
}

async function render() {
    let s;
    try {
        s = await chrome.runtime.sendMessage({ type: 'status' });
    } catch {
        // No listener: the service worker failed to start (or was just updated).
        setPill('off', 'Not running');
        problem(
            '<b>The extension\'s worker isn\'t running.</b> Press reload on Smooth in <code>chrome://extensions</code>; if it stays off, its "Errors" button says why.',
        );
        $('browsingCard').classList.add('disabled');
        $('trustedCard').classList.add('disabled');
        return;
    }
    $('browsingCard').classList.remove('disabled');
    if (s.connected) {
        setPill('on', 'Connected');
        problem(null);
    } else {
        setPill('off', 'Not connected');
        const why = s.lastError ? `<br>${escape(s.lastError)}` : '';
        problem(`<b>Can't reach th.</b> Run <code>th chrome install</code>, then reload this extension.${why}`);
    }

    const browsing = !!s.browsing;
    const trusted = await chrome.permissions.contains(TRUSTED);

    $('browsing').disabled = false;
    $('browsing').checked = browsing;
    $('browsingState').textContent = browsing ? 'On' : 'Off';
    $('browsing').onchange = (e) => toggle(BROWSING, e.target.checked);

    // Trusted input builds on browsing; it can always be turned off.
    const canTrust = browsing || trusted;
    $('trusted').disabled = !canTrust;
    $('trusted').checked = trusted;
    $('trustedState').textContent = trusted ? 'On' : canTrust ? 'Off' : 'Needs browsing';
    $('trustedCard').classList.toggle('disabled', !canTrust);
    $('trusted').onchange = (e) => toggle(TRUSTED, e.target.checked);
}

render();

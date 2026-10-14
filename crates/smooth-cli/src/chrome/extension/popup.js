const BROWSING = { permissions: ['tabs', 'scripting', 'tabGroups'], origins: ['<all_urls>'] };
const TRUSTED = { permissions: ['debugger'] };
const $ = (id) => document.getElementById(id);

async function render() {
    const s = await chrome.runtime.sendMessage({ type: 'status' });
    $('dot').className = `dot ${s.connected ? 'on' : 'off'}`;
    $('conn').textContent = s.connected ? 'Connected to th' : 'Not connected to th';
    $('err').textContent = s.connected ? '' : `${s.lastError ?? ''} — run: th chrome install`;
    $('browsing').textContent = s.browsing ? 'on' : 'off';
    $('toggle').hidden = false;
    $('toggle').textContent = s.browsing ? 'Disable browsing' : 'Enable browsing';
    $('toggle').onclick = async () => {
        if (s.browsing) await chrome.permissions.remove(BROWSING);
        else await chrome.permissions.request(BROWSING);
        render();
    };
    const trusted = await chrome.permissions.contains(TRUSTED);
    $('trusted').textContent = trusted ? 'on' : 'off';
    $('trustedToggle').hidden = !s.browsing && !trusted;
    $('trustedToggle').textContent = trusted ? 'Disable trusted input' : 'Enable trusted input';
    $('trustedToggle').onclick = async () => {
        if (trusted) await chrome.permissions.remove(TRUSTED);
        else await chrome.permissions.request(TRUSTED);
        render();
    };
}

render();

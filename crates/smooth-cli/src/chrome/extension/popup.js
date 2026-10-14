const BROWSING = { permissions: ['tabs', 'scripting', 'tabGroups'], origins: ['<all_urls>'] };
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
}

render();

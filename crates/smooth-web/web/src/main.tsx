import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';

import './globals.css';
import App from './App';
import { fetchPeers, readRemembered, resolveComputer, setActiveComputer, windowTitle } from './computers';
import { resolveLocalTarget } from './operator';
import { PWAUpdater } from './PWAUpdater';

// iOS pinch-zoom lock (th-086d97). Safari has ignored `user-scalable=no` since
// iOS 10 and `touch-action` doesn't cover the document pinch gesture, so these
// non-standard WebKit `gesture*` events are the only way to keep the app shell
// from being zoomed and panned off-screen. Other engines never fire them.
for (const evt of ['gesturestart', 'gesturechange', 'gestureend']) {
    document.addEventListener(evt, (e) => e.preventDefault(), { passive: false });
}

// Which computer does this window drive (th-a49e21)? This one unless the
// computer switcher remembered another — and then only if the relay lists it
// online right now, so a sleeping smoo-hub never leaves the window dead: it
// comes up on this computer and says why. Decided ONCE, before the first render,
// because every client (the WS, `/cd`, Stats, `@`-search) reads the same target.
async function chooseComputer(): Promise<void> {
    const remembered = readRemembered(localStorage);
    if (!remembered) return;
    const { http, token } = resolveLocalTarget();
    const { active, fallback } = resolveComputer(remembered, await fetchPeers(http, token, 3000));
    setActiveComputer(active, fallback);
    document.title = windowTitle(document.title, active);
}

// smooth-web is the operator's control surface — a thin client on the canonical
// WS protocol (EPIC th-c89c2a, th-f1a1f0). No more backend-detection split: the
// operator daemon is the one backend.
void chooseComputer()
    .catch(() => setActiveComputer({ kind: 'local' }))
    .finally(() =>
        createRoot(document.getElementById('root')!).render(
            <StrictMode>
                <App />
                <PWAUpdater />
            </StrictMode>,
        ),
    );

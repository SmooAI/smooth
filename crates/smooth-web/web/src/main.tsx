import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';

import './globals.css';
import App from './App';
import { PWAUpdater } from './PWAUpdater';

// iOS pinch-zoom lock (th-086d97). Safari has ignored `user-scalable=no` since
// iOS 10 and `touch-action` doesn't cover the document pinch gesture, so these
// non-standard WebKit `gesture*` events are the only way to keep the app shell
// from being zoomed and panned off-screen. Other engines never fire them.
for (const evt of ['gesturestart', 'gesturechange', 'gestureend']) {
    document.addEventListener(evt, (e) => e.preventDefault(), { passive: false });
}

// smooth-web is the operator's control surface — a thin client on the canonical
// WS protocol (EPIC th-c89c2a, th-f1a1f0). No more backend-detection split: the
// operator daemon is the one backend.
createRoot(document.getElementById('root')!).render(
    <StrictMode>
        <App />
        <PWAUpdater />
    </StrictMode>,
);

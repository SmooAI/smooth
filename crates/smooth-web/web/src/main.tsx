import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';

import './globals.css';
import App from './App';

// smooth-web is the operator's control surface — a thin client on the canonical
// WS protocol (EPIC th-c89c2a, th-f1a1f0). No more backend-detection split: the
// operator daemon is the one backend.
createRoot(document.getElementById('root')!).render(
    <StrictMode>
        <App />
    </StrictMode>,
);

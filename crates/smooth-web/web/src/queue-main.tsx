// `th ci-queue web` — the check queue as its own page (SMOODEV-3371). The same
// view as Big Smooth's Queue tab, fed by Server-Sent Events from the `th`
// process that served it, with the whole window to itself. No daemon needed.

import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';

import './globals.css';
import CiQueuePage from './CiQueuePage';

function Standalone() {
    return (
        <div className="flex h-dvh flex-col">
            <div className="mx-auto flex min-h-0 w-full max-w-[1800px] flex-1 flex-col">
                <CiQueuePage source="stream" standalone />
            </div>
        </div>
    );
}

createRoot(document.getElementById('root')!).render(
    <StrictMode>
        <Standalone />
    </StrictMode>,
);

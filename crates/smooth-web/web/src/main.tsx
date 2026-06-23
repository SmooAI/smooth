import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';
import { BrowserRouter, Route, Routes } from 'react-router-dom';

import './globals.css';
import { ControlApp } from './control';
import { ProjectProvider } from './context';
import { getHealth } from './daemon';
import { Layout } from './layout';
import { ChatPage } from './pages/chat';
import { DashboardPage } from './pages/dashboard';
import { OperatorsPage } from './pages/operators';
import { PearlsPage } from './pages/pearls';
import { SystemPage } from './pages/system';

// The legacy Big Smooth SPA.
function LegacyApp() {
    return (
        <BrowserRouter>
            <ProjectProvider>
                <Routes>
                    <Route element={<Layout />}>
                        <Route path="/" element={<DashboardPage />} />
                        <Route path="/pearls" element={<PearlsPage />} />
                        <Route path="/operators" element={<OperatorsPage />} />
                        <Route path="/chat" element={<ChatPage />} />
                        <Route path="/system" element={<SystemPage />} />
                    </Route>
                </Routes>
            </ProjectProvider>
        </BrowserRouter>
    );
}

// Detect which backend is serving us: the always-on daemon gets the control
// surface; Big Smooth gets the legacy SPA (EPIC th-c89c2a / th-bd0def).
async function boot() {
    let isDaemon = false;
    try {
        const h = await getHealth();
        isDaemon = h.service === 'smooth-daemon';
    } catch {
        /* default to the legacy app if /health is unreachable */
    }
    createRoot(document.getElementById('root')!).render(<StrictMode>{isDaemon ? <ControlApp /> : <LegacyApp />}</StrictMode>);
}

void boot();

import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';
import { BrowserRouter, Route, Routes } from 'react-router-dom';

import './globals.css';
import { Layout } from './layout';
import { DashboardPage } from './pages/dashboard';
import { PearlsPage } from './pages/pearls';
import { OperatorsPage } from './pages/operators';
import { ChatPage } from './pages/chat';
import { SystemPage } from './pages/system';

createRoot(document.getElementById('root')!).render(
    <StrictMode>
        <BrowserRouter>
            <Routes>
                <Route element={<Layout />}>
                    <Route path="/" element={<DashboardPage />} />
                    <Route path="/pearls" element={<PearlsPage />} />
                    <Route path="/operators" element={<OperatorsPage />} />
                    <Route path="/chat" element={<ChatPage />} />
                    <Route path="/system" element={<SystemPage />} />
                </Route>
            </Routes>
        </BrowserRouter>
    </StrictMode>,
);

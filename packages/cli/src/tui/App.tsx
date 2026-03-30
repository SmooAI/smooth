import { Box, useInput } from 'ink';
import React, { useCallback, useState } from 'react';

import { LeaderClient } from '../client/leader-client.js';
import { getActiveServerUrl, getApiKey } from '../config.js';
import { Header } from './components/Header.js';
import { StatusBar } from './components/StatusBar.js';
import { useMouse } from './hooks/useMouse.js';
import { BeadsView } from './views/BeadsView.js';
import { ChatView } from './views/ChatView.js';
import { DashboardView } from './views/DashboardView.js';
import { MessagesView } from './views/MessagesView.js';
import { OperatorsView } from './views/OperatorsView.js';
import { ProjectsView } from './views/ProjectsView.js';
import { ReviewsView } from './views/ReviewsView.js';
import { SystemView } from './views/SystemView.js';

const TABS = ['Dashboard', 'Projects', 'Beads', 'Operators', 'Chat', 'Messages', 'Reviews', 'System'] as const;
type Tab = (typeof TABS)[number];

interface AppProps {
    serverUrl?: string;
}

export function App({ serverUrl }: AppProps) {
    const url = serverUrl ?? getActiveServerUrl();
    const client = new LeaderClient(url, getApiKey(url));
    const [activeTab, setActiveTab] = useState<Tab>('Dashboard');

    // Mouse support — click header tabs to switch views
    const handleMouse = useCallback(
        (event: { x: number; y: number; button: string; type: string }) => {
            if (event.type !== 'press' || event.button !== 'left') return;

            // Tab bar is on y=3 (inside header border)
            if (event.y === 3) {
                let col = 2;
                for (const tab of TABS) {
                    const label = `${TABS.indexOf(tab) + 1}:${tab}`;
                    if (event.x >= col && event.x < col + label.length + 1) {
                        setActiveTab(tab);
                        return;
                    }
                    col += label.length + 1;
                }
            }
        },
        [],
    );

    useMouse(handleMouse);

    useInput((input, key) => {
        if (key.tab) {
            const idx = TABS.indexOf(activeTab);
            const next = key.shift ? TABS[(idx - 1 + TABS.length) % TABS.length] : TABS[(idx + 1) % TABS.length];
            setActiveTab(next);
        }

        const num = parseInt(input, 10);
        if (num >= 1 && num <= TABS.length) {
            setActiveTab(TABS[num - 1]);
        }

        if (input === '/') setActiveTab('Chat');
        if (input === 'q' && activeTab !== 'Chat') process.exit(0);
    });

    return (
        <Box flexDirection="column" width="100%">
            <Header serverUrl={url} activeTab={activeTab} tabs={[...TABS]} />
            <Box flexDirection="column" paddingX={1} flexGrow={1}>
                {activeTab === 'Dashboard' && <DashboardView client={client} />}
                {activeTab === 'Projects' && <ProjectsView client={client} />}
                {activeTab === 'Beads' && <BeadsView client={client} />}
                {activeTab === 'Operators' && <OperatorsView client={client} />}
                {activeTab === 'Chat' && <ChatView client={client} />}
                {activeTab === 'Messages' && <MessagesView client={client} />}
                {activeTab === 'Reviews' && <ReviewsView client={client} />}
                {activeTab === 'System' && <SystemView client={client} />}
            </Box>
            <StatusBar activeTab={activeTab} />
        </Box>
    );
}

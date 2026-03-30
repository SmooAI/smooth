import { Box, Text } from 'ink';
import React from 'react';

interface HeaderProps {
    serverUrl: string;
    activeTab: string;
    tabs: string[];
}

// Smoo AI brand: green/teal primary (#00a6a6), orange accent (#f49f0a)
const SMOO_GREEN = '#00a6a6';

export function Header({ serverUrl, activeTab, tabs }: HeaderProps) {
    return (
        <Box flexDirection="column" borderStyle="single" borderColor={SMOO_GREEN} paddingX={1}>
            <Box justifyContent="space-between">
                <Text bold color={SMOO_GREEN}>
                    SMOO.AI / SMOOTH
                </Text>
                <Text dimColor>{serverUrl}</Text>
            </Box>
            <Box gap={1}>
                {tabs.map((tab, i) => (
                    <Text key={tab} color={tab === activeTab ? SMOO_GREEN : undefined} bold={tab === activeTab} dimColor={tab !== activeTab}>
                        {i + 1}:{tab}
                    </Text>
                ))}
            </Box>
        </Box>
    );
}

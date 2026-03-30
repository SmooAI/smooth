import React from 'react';
import { Box, Text } from 'ink';

interface HeaderProps {
    serverUrl: string;
    activeTab: string;
    tabs: string[];
}

export function Header({ serverUrl, activeTab, tabs }: HeaderProps) {
    return (
        <Box flexDirection="column" borderStyle="single" borderColor="cyan" paddingX={1}>
            <Box justifyContent="space-between">
                <Text bold color="cyan">
                    SMOOTH
                </Text>
                <Text dimColor>{serverUrl}</Text>
            </Box>
            <Box gap={1}>
                {tabs.map((tab, i) => (
                    <Text key={tab} color={tab === activeTab ? 'cyan' : undefined} bold={tab === activeTab} dimColor={tab !== activeTab}>
                        {i + 1}:{tab}
                    </Text>
                ))}
            </Box>
        </Box>
    );
}

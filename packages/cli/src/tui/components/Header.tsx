import { Box, Text } from 'ink';
import BigText from 'ink-big-text';
import Gradient from 'ink-gradient';
import React from 'react';

interface HeaderProps {
    serverUrl: string;
    activeTab: string;
    tabs: string[];
}

// Smoo AI brand colors (from globals.css)
const SMOO_GREEN = '#00a6a6';
const SMOO_ORANGE = '#f49f0a';

export function Header({ serverUrl, activeTab, tabs }: HeaderProps) {
    return (
        <Box flexDirection="column" paddingX={1}>
            <Box justifyContent="space-between" alignItems="center">
                <Gradient colors={[SMOO_ORANGE, '#ff6b6c']}>
                    <BigText text="Smoo AI" font="tiny" />
                </Gradient>
                <Box flexDirection="column" alignItems="flex-end">
                    <Text color={SMOO_GREEN} bold>
                        SMOOTH
                    </Text>
                    <Text dimColor>{serverUrl}</Text>
                </Box>
            </Box>
            <Box gap={1} borderStyle="single" borderColor={SMOO_GREEN} paddingX={1}>
                {tabs.map((tab, i) => (
                    <Text key={tab} color={tab === activeTab ? SMOO_ORANGE : undefined} bold={tab === activeTab} dimColor={tab !== activeTab}>
                        {i + 1}:{tab}
                    </Text>
                ))}
            </Box>
        </Box>
    );
}

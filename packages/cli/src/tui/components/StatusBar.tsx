import React from 'react';
import { Box, Text } from 'ink';

interface StatusBarProps {
    activeTab: string;
}

export function StatusBar({ activeTab }: StatusBarProps) {
    const hints = activeTab === 'Chat' ? 'Type to chat | @file search | Esc to unfocus | Tab to switch' : 'Tab/Shift+Tab: switch | 1-8: jump | /: chat | j/k: navigate | q: quit';

    return (
        <Box borderStyle="single" borderColor="gray" paddingX={1}>
            <Text dimColor>{hints}</Text>
        </Box>
    );
}

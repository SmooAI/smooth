import { Box, Text } from 'ink';
import React, { useEffect, useState } from 'react';

import type { LeaderClient } from '../../client/leader-client.js';

interface Props {
    client: LeaderClient;
}

export function DashboardView({ client }: Props) {
    const [health, setHealth] = useState<Record<string, unknown> | null>(null);
    const [error, setError] = useState<string | null>(null);

    useEffect(() => {
        client
            .getHealth()
            .then((h) => setHealth(h as unknown as Record<string, unknown>))
            .catch((e) => setError((e as Error).message));
    }, [client]);

    if (error) {
        return (
            <Box flexDirection="column">
                <Text bold color="red">
                    Cannot reach leader
                </Text>
                <Text dimColor>{error}</Text>
            </Box>
        );
    }

    if (!health) {
        return <Text dimColor>Loading...</Text>;
    }

    return (
        <Box flexDirection="column" gap={1}>
            <Text bold>Dashboard</Text>
            <Box flexDirection="column">
                <Text color="green">Leader: healthy</Text>
                <Text>Uptime: {Math.round((health as { uptime?: number }).uptime ?? 0)}s</Text>
            </Box>
            <Text dimColor>Press Tab to navigate views</Text>
        </Box>
    );
}

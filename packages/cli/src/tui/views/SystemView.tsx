import React, { useEffect, useState } from 'react';
import { Box, Text } from 'ink';

import type { LeaderClient } from '../../client/leader-client.js';

interface Props {
    client: LeaderClient;
}

export function SystemView({ client }: Props) {
    const [health, setHealth] = useState<any>(null);
    const [loading, setLoading] = useState(true);

    useEffect(() => {
        client
            .getSystemHealth()
            .then((r) => setHealth(r.data))
            .catch(() => {})
            .finally(() => setLoading(false));
    }, []);

    if (loading) return <Text dimColor>Loading system health...</Text>;

    const statusIcon = (s: string) => (s === 'healthy' || s === 'connected' ? '●' : s === 'degraded' ? '◐' : '○');
    const statusColor = (s: string) => (s === 'healthy' || s === 'connected' ? 'green' : s === 'degraded' ? 'yellow' : 'red');

    return (
        <Box flexDirection="column" gap={1}>
            <Text bold>System Health</Text>
            {health ? (
                <Box flexDirection="column">
                    <Text color={statusColor(health.leader?.status)}>
                        {statusIcon(health.leader?.status)} Leader: {health.leader?.status} (uptime: {Math.round(health.leader?.uptime ?? 0)}s)
                    </Text>
                    <Text color={statusColor(health.database?.status)}>
                        {statusIcon(health.database?.status)} Database: {health.database?.status} ({health.database?.path ?? 'unknown'})
                    </Text>
                    <Text color={statusColor(health.sandbox?.status)}>
                        {statusIcon(health.sandbox?.status)} Sandbox: {health.sandbox?.status} ({health.sandbox?.backend ?? 'unknown'}, {health.sandbox?.activeSandboxes ?? 0}/{health.sandbox?.maxConcurrency ?? 0})
                    </Text>
                    <Text color={statusColor(health.tailscale?.status)}>
                        {statusIcon(health.tailscale?.status)} Tailscale: {health.tailscale?.status}
                        {health.tailscale?.hostname ? ` (${health.tailscale.hostname})` : ''}
                    </Text>
                    <Text color={statusColor(health.beads?.status)}>
                        {statusIcon(health.beads?.status)} Beads: {health.beads?.status} ({health.beads?.openIssues ?? 0} open)
                    </Text>
                </Box>
            ) : (
                <Text color="red">Cannot reach system health endpoint</Text>
            )}
        </Box>
    );
}

import { Box, Text } from 'ink';
import React, { useCallback, useEffect, useState } from 'react';

import type { LeaderClient } from '../../client/leader-client.js';

interface Props {
    client: LeaderClient;
}

export function DashboardView({ client }: Props) {
    const [health, setHealth] = useState<any>(null);
    const [operators, setOperators] = useState<any[]>([]);
    const [reviews, setReviews] = useState<any[]>([]);
    const [inbox, setInbox] = useState<any[]>([]);
    const [error, setError] = useState<string | null>(null);

    const load = useCallback(() => {
        client
            .getSystemHealth()
            .then((r) => setHealth(r.data))
            .catch((e) => setError((e as Error).message));
        client
            .listOperators()
            .then((r) => setOperators(r.data as any[]))
            .catch(() => {});
        client
            .getPendingReviews()
            .then((r) => setReviews(r.data as any[]))
            .catch(() => {});
        client
            .getInbox()
            .then((r) => setInbox(r.data as any[]))
            .catch(() => {});
    }, [client]);

    useEffect(() => {
        load();
        const interval = setInterval(load, 5000);
        return () => clearInterval(interval);
    }, [load]);

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

    if (!health) return <Text dimColor>Loading...</Text>;

    const statusIcon = (s: string) => (s === 'healthy' || s === 'connected' ? '●' : s === 'degraded' ? '◐' : '○');
    const statusColor = (s: string): string | undefined => (s === 'healthy' || s === 'connected' ? 'green' : s === 'degraded' ? 'yellow' : 'red');

    return (
        <Box flexDirection="column" gap={1}>
            <Text bold>Dashboard</Text>

            {/* Health overview */}
            <Box flexDirection="column">
                <Text color={statusColor(health.leader?.status)}>
                    {statusIcon(health.leader?.status)} Leader: {health.leader?.status} (uptime: {Math.round(health.leader?.uptime ?? 0)}s)
                </Text>
                <Text color={statusColor(health.database?.status)}>
                    {statusIcon(health.database?.status)} Database: {health.database?.status}
                </Text>
                <Text color={statusColor(health.sandbox?.status)}>
                    {statusIcon(health.sandbox?.status)} Sandbox: {health.sandbox?.backend ?? 'unknown'} ({health.sandbox?.activeSandboxes ?? 0}/
                    {health.sandbox?.maxConcurrency ?? 0})
                </Text>
                <Text color={statusColor(health.tailscale?.status)}>
                    {statusIcon(health.tailscale?.status)} Tailscale: {health.tailscale?.status}
                </Text>
            </Box>

            {/* Active work summary */}
            <Box flexDirection="column">
                <Text bold>Activity</Text>
                <Text>
                    {operators.length} operator{operators.length !== 1 ? 's' : ''} active
                    {reviews.length > 0 && (
                        <Text color="yellow">
                            {' '}
                            | {reviews.length} review{reviews.length !== 1 ? 's' : ''} pending
                        </Text>
                    )}
                    {inbox.length > 0 && (
                        <Text color="cyan">
                            {' '}
                            | {inbox.length} message{inbox.length !== 1 ? 's' : ''}
                        </Text>
                    )}
                </Text>
            </Box>

            {/* Active operators */}
            {operators.length > 0 && (
                <Box flexDirection="column">
                    {operators.slice(0, 5).map((op: any, i) => (
                        <Box key={i} gap={1}>
                            <Text color="cyan">{op.workerId ?? op.id}</Text>
                            <Text color={op.phase === 'execute' ? 'yellow' : undefined}>[{op.phase}]</Text>
                            <Text dimColor>{op.beadId}</Text>
                        </Box>
                    ))}
                    {operators.length > 5 && <Text dimColor>...and {operators.length - 5} more</Text>}
                </Box>
            )}

            <Text dimColor>Press Tab to switch views | Auto-refresh: 5s</Text>
        </Box>
    );
}

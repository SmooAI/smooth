import { Box, Text, useInput } from 'ink';
import React, { useEffect, useState } from 'react';

import type { LeaderClient } from '../../client/leader-client.js';

interface Props {
    client: LeaderClient;
}

export function BeadsView({ client }: Props) {
    const [beads, setBeads] = useState<any[]>([]);
    const [selected, setSelected] = useState(0);
    const [loading, setLoading] = useState(true);

    useEffect(() => {
        client
            .listBeads()
            .then((r) => setBeads(r.data as any[]))
            .catch(() => {})
            .finally(() => setLoading(false));
    }, [client]);

    useInput((input, key) => {
        if (input === 'j' || key.downArrow) setSelected((s) => Math.min(s + 1, beads.length - 1));
        if (input === 'k' || key.upArrow) setSelected((s) => Math.max(s - 1, 0));
    });

    if (loading) return <Text dimColor>Loading beads...</Text>;

    return (
        <Box flexDirection="column" gap={1}>
            <Text bold>Beads ({beads.length})</Text>
            {beads.length === 0 && <Text dimColor>No beads found.</Text>}
            {beads.map((b: any, i) => {
                const isSelected = i === selected;
                const statusColor = b.status === 'closed' ? 'green' : b.status === 'blocked' ? 'red' : b.status === 'in_progress' ? 'yellow' : undefined;
                return (
                    <Box key={i} gap={1}>
                        <Text color={isSelected ? 'cyan' : undefined} bold={isSelected}>
                            {isSelected ? '>' : ' '}
                        </Text>
                        <Text color={statusColor}>[{b.status}]</Text>
                        <Text color="cyan">{b.id}</Text>
                        <Text>{b.title}</Text>
                        <Text dimColor>P{b.priority}</Text>
                    </Box>
                );
            })}
        </Box>
    );
}

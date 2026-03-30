import { Box, Text } from 'ink';
import React, { useEffect, useState } from 'react';

import type { LeaderClient } from '../../client/leader-client.js';

interface Props {
    client: LeaderClient;
}

export function OperatorsView({ client }: Props) {
    const [operators, setOperators] = useState<any[]>([]);
    const [loading, setLoading] = useState(true);

    useEffect(() => {
        client
            .listOperators()
            .then((r) => setOperators(r.data as any[]))
            .catch(() => {})
            .finally(() => setLoading(false));
    }, [client]);

    if (loading) return <Text dimColor>Loading Smooth Operators...</Text>;

    return (
        <Box flexDirection="column" gap={1}>
            <Text bold>Smooth Operators ({operators.length})</Text>
            {operators.length === 0 && <Text dimColor>No active Smooth Operators.</Text>}
            {operators.map((op: any, i) => {
                const phaseColor = op.phase === 'execute' ? 'yellow' : op.phase === 'finalize' ? 'green' : undefined;
                return (
                    <Box key={i} gap={1}>
                        <Text color="cyan">{op.workerId ?? op.id}</Text>
                        <Text color={phaseColor}>[{op.phase}]</Text>
                        <Text>bead: {op.beadId}</Text>
                        <Text dimColor>{op.status}</Text>
                    </Box>
                );
            })}
        </Box>
    );
}

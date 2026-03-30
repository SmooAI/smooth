import { Box, Text, useInput } from 'ink';
import React, { useCallback, useEffect, useState } from 'react';

import type { LeaderClient } from '../../client/leader-client.js';

interface Props {
    client: LeaderClient;
}

export function OperatorsView({ client }: Props) {
    const [operators, setOperators] = useState<any[]>([]);
    const [loading, setLoading] = useState(true);
    const [selected, setSelected] = useState(0);
    const [actionMsg, setActionMsg] = useState('');

    const load = useCallback(() => {
        client
            .listOperators()
            .then((r) => setOperators(r.data as any[]))
            .catch(() => {})
            .finally(() => setLoading(false));
    }, [client]);

    useEffect(() => {
        load();
        const interval = setInterval(load, 5000);
        return () => clearInterval(interval);
    }, [load]);

    useInput(async (input, key) => {
        if (operators.length === 0) return;

        if (key.upArrow) setSelected((s) => Math.max(0, s - 1));
        if (key.downArrow) setSelected((s) => Math.min(operators.length - 1, s + 1));

        const op = operators[selected];
        if (!op) return;
        const beadId = op.beadId;

        if (input === 'p') {
            try {
                await client.pauseOperator(beadId);
                setActionMsg(`Paused operator on ${beadId}`);
            } catch (e) {
                setActionMsg(`Error: ${(e as Error).message}`);
            }
            load();
        }
        if (input === 'r') {
            try {
                await client.resumeOperator(beadId);
                setActionMsg(`Resumed operator on ${beadId}`);
            } catch (e) {
                setActionMsg(`Error: ${(e as Error).message}`);
            }
            load();
        }
        if (input === 'x') {
            try {
                await client.cancelOperator(beadId);
                setActionMsg(`Cancelled operator on ${beadId}`);
            } catch (e) {
                setActionMsg(`Error: ${(e as Error).message}`);
            }
            load();
        }
    });

    if (loading) return <Text dimColor>Loading Smooth Operators...</Text>;

    return (
        <Box flexDirection="column" gap={1}>
            <Text bold>Smooth Operators ({operators.length})</Text>
            {operators.length === 0 && <Text dimColor>No active Smooth Operators.</Text>}
            {operators.map((op: any, i) => {
                const isSelected = i === selected;
                const phaseColor = op.phase === 'execute' ? 'yellow' : op.phase === 'finalize' ? 'green' : undefined;
                const isPaused = op.status === 'paused' || (op.metadata?.labels ?? []).includes('steering:paused');

                return (
                    <Box key={i} gap={1}>
                        <Text color={isSelected ? 'cyan' : undefined}>{isSelected ? '>' : ' '}</Text>
                        <Text color="cyan">{op.workerId ?? op.id}</Text>
                        <Text color={phaseColor}>[{op.phase}]</Text>
                        <Text>bead: {op.beadId}</Text>
                        {isPaused ? <Text color="yellow">PAUSED</Text> : <Text dimColor>{op.status}</Text>}
                    </Box>
                );
            })}
            {operators.length > 0 && (
                <Box marginTop={1}>
                    <Text dimColor>Keys: [p]ause  [r]esume  [x] cancel  [arrows] navigate</Text>
                </Box>
            )}
            {actionMsg && <Text color="green">{actionMsg}</Text>}
        </Box>
    );
}

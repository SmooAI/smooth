import { Box, Text, useInput } from 'ink';
import React, { useCallback, useEffect, useState } from 'react';

import type { LeaderClient } from '../../client/leader-client.js';

interface Props {
    client: LeaderClient;
}

export function ReviewsView({ client }: Props) {
    const [reviews, setReviews] = useState<any[]>([]);
    const [loading, setLoading] = useState(true);
    const [selected, setSelected] = useState(0);
    const [actionMsg, setActionMsg] = useState('');

    const load = useCallback(() => {
        client
            .getPendingReviews()
            .then((r) => setReviews(r.data as any[]))
            .catch(() => {})
            .finally(() => setLoading(false));
    }, [client]);

    useEffect(() => {
        load();
        const interval = setInterval(load, 10000);
        return () => clearInterval(interval);
    }, [load]);

    useInput(async (input, key) => {
        if (reviews.length === 0) return;

        if (key.upArrow) setSelected((s) => Math.max(0, s - 1));
        if (key.downArrow) setSelected((s) => Math.min(reviews.length - 1, s + 1));

        const review = reviews[selected];
        if (!review) return;
        const beadId = review.id ?? review.beadId;

        if (input === 'a') {
            try {
                await client.approveReview(beadId);
                setActionMsg(`Approved ${beadId}`);
            } catch (e) {
                setActionMsg(`Error: ${(e as Error).message}`);
            }
            load();
        }
        if (input === 'r') {
            try {
                await client.requestRework(beadId, 'Rework requested from TUI');
                setActionMsg(`Rework requested for ${beadId}`);
            } catch (e) {
                setActionMsg(`Error: ${(e as Error).message}`);
            }
            load();
        }
    });

    if (loading) return <Text dimColor>Loading reviews...</Text>;

    return (
        <Box flexDirection="column" gap={1}>
            <Text bold>Pending Reviews ({reviews.length})</Text>
            {reviews.length === 0 && <Text dimColor>No pending reviews.</Text>}
            {reviews.map((r: any, i) => {
                const isSelected = i === selected;
                return (
                    <Box key={i} gap={1}>
                        <Text color={isSelected ? 'cyan' : undefined}>{isSelected ? '>' : ' '}</Text>
                        <Text color="cyan">{r.id ?? r.beadId}</Text>
                        <Text>{r.title}</Text>
                        <Text color="yellow">[pending]</Text>
                    </Box>
                );
            })}
            {reviews.length > 0 && (
                <Box marginTop={1}>
                    <Text dimColor>Keys: [a]pprove  [r]ework  [arrows] navigate</Text>
                </Box>
            )}
            {actionMsg && <Text color="green">{actionMsg}</Text>}
        </Box>
    );
}

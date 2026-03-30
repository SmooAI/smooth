import { Box, Text } from 'ink';
import React, { useEffect, useState } from 'react';

import type { LeaderClient } from '../../client/leader-client.js';

interface Props {
    client: LeaderClient;
}

export function ReviewsView({ client }: Props) {
    const [reviews, setReviews] = useState<any[]>([]);
    const [loading, setLoading] = useState(true);

    useEffect(() => {
        client
            .getPendingReviews()
            .then((r) => setReviews(r.data as any[]))
            .catch(() => {})
            .finally(() => setLoading(false));
    }, [client]);

    if (loading) return <Text dimColor>Loading reviews...</Text>;

    return (
        <Box flexDirection="column" gap={1}>
            <Text bold>Pending Reviews ({reviews.length})</Text>
            {reviews.length === 0 && <Text dimColor>No pending reviews.</Text>}
            {reviews.map((r: any, i) => (
                <Box key={i} gap={1}>
                    <Text color="cyan">{r.id ?? r.beadId}</Text>
                    <Text>{r.title}</Text>
                    <Text color="yellow">[pending]</Text>
                </Box>
            ))}
            {reviews.length > 0 && <Text dimColor>Approve: th approve &lt;beadId&gt;</Text>}
        </Box>
    );
}

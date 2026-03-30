import { Box, Text } from 'ink';
import React, { useEffect, useState } from 'react';

import type { LeaderClient } from '../../client/leader-client.js';

interface Props {
    client: LeaderClient;
}

export function MessagesView({ client }: Props) {
    const [inbox, setInbox] = useState<any[]>([]);
    const [loading, setLoading] = useState(true);

    useEffect(() => {
        client
            .getInbox()
            .then((r) => setInbox(r.data as any[]))
            .catch(() => {})
            .finally(() => setLoading(false));
    }, [client]);

    if (loading) return <Text dimColor>Loading messages...</Text>;

    return (
        <Box flexDirection="column" gap={1}>
            <Text bold>Inbox ({inbox.length})</Text>
            {inbox.length === 0 && <Text dimColor>No messages requiring attention.</Text>}
            {inbox.map((item: any, i) => (
                <Box key={i} flexDirection="column" borderStyle="single" borderColor="gray" paddingX={1}>
                    <Box gap={1}>
                        <Text color="cyan">{item.message?.beadId}</Text>
                        <Text bold>{item.beadTitle}</Text>
                        {item.requiresAction && <Text color="yellow">[{item.actionType}]</Text>}
                    </Box>
                    <Text>{item.message?.content}</Text>
                </Box>
            ))}
        </Box>
    );
}

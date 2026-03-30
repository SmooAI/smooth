import { Box, Text } from 'ink';
import React, { useEffect, useState } from 'react';

import type { LeaderClient } from '../../client/leader-client.js';

interface Props {
    client: LeaderClient;
}

export function ProjectsView({ client }: Props) {
    const [projects, setProjects] = useState<unknown[]>([]);
    const [loading, setLoading] = useState(true);

    useEffect(() => {
        client
            .listProjects()
            .then((r) => setProjects(r.data))
            .catch(() => {})
            .finally(() => setLoading(false));
    }, [client]);

    if (loading) return <Text dimColor>Loading projects...</Text>;

    return (
        <Box flexDirection="column" gap={1}>
            <Text bold>Projects ({projects.length})</Text>
            {projects.length === 0 && <Text dimColor>No projects. Create one: th project create &lt;name&gt;</Text>}
            {projects.map((p: any, i) => (
                <Box key={i} gap={1}>
                    <Text color="cyan">{p.id ?? p.title ?? 'unknown'}</Text>
                    <Text>{p.title ?? p.name ?? ''}</Text>
                    <Text dimColor>[{p.status ?? 'open'}]</Text>
                </Box>
            ))}
        </Box>
    );
}

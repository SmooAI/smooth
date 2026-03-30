import { Hono } from 'hono';
import { streamSSE } from 'hono/streaming';

import { getEventStream } from '../backend/registry.js';

export const streamRoutes = new Hono();

/** SSE endpoint for live system updates — wired to ExecutionBackend EventStream */
streamRoutes.get('/', async (c) => {
    return streamSSE(c, async (stream) => {
        // Send initial connection event
        await stream.writeSSE({
            event: 'connected',
            data: JSON.stringify({
                type: 'system_health',
                data: { status: 'connected' },
                timestamp: new Date().toISOString(),
            }),
        });

        // Subscribe to execution backend events
        const events = getEventStream();
        const unsubscribe = events.on('*', (event) => {
            stream
                .writeSSE({
                    event: event.type,
                    data: JSON.stringify({
                        type: event.type,
                        sandboxId: event.sandboxId,
                        operatorId: event.operatorId,
                        beadId: event.beadId,
                        data: event.data,
                        timestamp: event.timestamp.toISOString(),
                    }),
                })
                .catch(() => {
                    // Stream closed
                });
        });

        // Heartbeat
        const interval = setInterval(async () => {
            try {
                await stream.writeSSE({
                    event: 'heartbeat',
                    data: JSON.stringify({
                        type: 'system_health',
                        data: { uptime: process.uptime() },
                        timestamp: new Date().toISOString(),
                    }),
                });
            } catch {
                clearInterval(interval);
            }
        }, 30_000);

        // Clean up on disconnect
        stream.onAbort(() => {
            clearInterval(interval);
            unsubscribe();
        });

        // Keep the stream open
        await new Promise(() => {});
    });
});

import { Hono } from 'hono';
import { streamSSE } from 'hono/streaming';

export const streamRoutes = new Hono();

/** SSE endpoint for live system updates */
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

        // Keep connection alive with heartbeat
        // TODO: Wire up actual events from orchestrator (worker status, bead updates, etc.)
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
        });

        // Keep the stream open
        await new Promise(() => {});
    });
});

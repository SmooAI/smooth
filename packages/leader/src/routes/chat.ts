import { Hono } from 'hono';
import { streamSSE } from 'hono/streaming';

import { ChatMessageSchema } from '@smooth/shared/schemas';

export const chatRoutes = new Hono();

/** Send a chat message to the leader and get a streaming response */
chatRoutes.post('/', async (c) => {
    const body = await c.req.json();
    const parsed = ChatMessageSchema.parse(body);

    // Stream the response using SSE
    return streamSSE(c, async (stream) => {
        // TODO: Wire up to LangGraph for actual orchestration
        // For now, return a simple acknowledgment

        await stream.writeSSE({
            event: 'text',
            data: JSON.stringify({
                type: 'text',
                content: `Received: "${parsed.content}". The leader orchestration graph will process this in Phase 2.`,
            }),
        });

        if (parsed.attachments?.length) {
            await stream.writeSSE({
                event: 'text',
                data: JSON.stringify({
                    type: 'text',
                    content: `Attached ${parsed.attachments.length} context item(s): ${parsed.attachments.map((a) => `${a.type}:${a.reference}`).join(', ')}`,
                }),
            });
        }

        await stream.writeSSE({
            event: 'done',
            data: JSON.stringify({ type: 'done', content: '' }),
        });
    });
});

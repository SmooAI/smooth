import { Hono } from 'hono';

export const healthRoutes = new Hono();

healthRoutes.get('/health', (c) => {
    return c.json({
        ok: true,
        service: 'smooth-leader',
        version: '0.1.0',
        uptime: process.uptime(),
        timestamp: new Date().toISOString(),
    });
});

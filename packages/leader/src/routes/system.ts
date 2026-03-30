import { Hono } from 'hono';

import { db } from '@smooth/db/client';
import { config } from '@smooth/db/schema/config';
import type { SystemHealth } from '@smooth/shared/types';

import { getBackend } from '../backend/registry.js';

export const systemRoutes = new Hono();

/** Get system health */
systemRoutes.get('/health', async (c) => {
    const backend = getBackend();

    const health: SystemHealth = {
        leader: { status: 'healthy', uptime: process.uptime() },
        postgres: { status: 'healthy', connectionCount: 0 },
        sandbox: {
            status: 'healthy',
            backend: backend.name,
            activeSandboxes: backend.activeCount(),
            maxConcurrency: backend.maxConcurrency(),
        },
        tailscale: { status: 'disconnected' },
        beads: { status: 'healthy', openIssues: 0 },
    };

    // Test PostgreSQL
    try {
        await db.select().from(config).limit(1);
        health.postgres.status = 'healthy';
    } catch {
        health.postgres.status = 'down';
    }

    // Test sandbox backend
    try {
        const hc = await backend.healthCheck();
        if (hc.unhealthy.length > 0) {
            health.sandbox.status = 'degraded';
        }
    } catch {
        health.sandbox.status = 'down';
    }

    return c.json({ data: health, ok: true });
});

/** Get system config */
systemRoutes.get('/config', async (c) => {
    const rows = await db.select().from(config);
    const configObj: Record<string, unknown> = {};
    for (const row of rows) {
        configObj[row.key] = row.value;
    }
    return c.json({ data: configObj, ok: true });
});

/** Set config value */
systemRoutes.put('/config', async (c) => {
    const body = await c.req.json();
    const { key, value } = body as { key: string; value: unknown };

    await db
        .insert(config)
        .values({ key, value, updatedAt: new Date() })
        .onConflictDoUpdate({ target: config.key, set: { value, updatedAt: new Date() } });

    return c.json({ ok: true });
});

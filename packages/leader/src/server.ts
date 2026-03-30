import { Hono } from 'hono';
import { cors } from 'hono/cors';
import { logger } from 'hono/logger';

import { serve } from '@hono/node-server';
import { getDbPath } from '@smooai/smooth-db/client';
import { ensureSchema } from '@smooai/smooth-db/migrate';
import { createAuditLogger, flushAuditLogs, getAuditDir } from '@smooai/smooth-shared/audit-log';

import { initializeBackend, shutdownBackend } from './backend/registry.js';
import { ensureBeadsDir, getBeadsDir } from './beads/client.js';
import { beadsRoutes } from './routes/beads.js';
import { chatRoutes } from './routes/chat.js';
import { execRoutes } from './routes/exec.js';
import { healthRoutes } from './routes/health.js';
import { jiraRoutes } from './routes/jira.js';
import { messagesRoutes } from './routes/messages.js';
import { projectsRoutes } from './routes/projects.js';
import { reviewsRoutes } from './routes/reviews.js';
import { steeringRoutes } from './routes/steering.js';
import { streamRoutes } from './routes/stream.js';
import { systemRoutes } from './routes/system.js';
import { workersRoutes } from './routes/workers.js';
import { wsApp, initWebSocket } from './routes/ws.js';
import { watchdog } from './workers/watchdog.js';

const app = new Hono();

// Middleware
app.use('*', logger());
app.use('*', cors());

// Routes
app.route('/', healthRoutes);
app.route('/api/projects', projectsRoutes);
app.route('/api/beads', beadsRoutes);
app.route('/api/workers', workersRoutes);
app.route('/api/messages', messagesRoutes);
app.route('/api/reviews', reviewsRoutes);
app.route('/api/system', systemRoutes);
app.route('/api/jira', jiraRoutes);
app.route('/api/chat', chatRoutes);
app.route('/api/stream', streamRoutes);
app.route('/api/workers', execRoutes);
app.route('/api/steering', steeringRoutes);
app.route('/', wsApp);

const port = parseInt(process.env.PORT ?? '4400', 10);

async function start() {
    console.log('Smooth leader starting...');

    // Auto-create SQLite tables
    ensureSchema();
    console.log(`Database: ${getDbPath()}`);

    // Ensure Beads directory exists
    ensureBeadsDir();
    console.log(`Beads: ${getBeadsDir()}`);

    // Initialize execution backend
    const backend = await initializeBackend();
    console.log(`Execution backend: ${backend.name}`);

    // Audit logging
    const leaderAudit = createAuditLogger('leader');
    leaderAudit.phaseStarted('startup');
    console.log(`Audit logs: ${getAuditDir()}`);

    // Make audit logger available globally for routes
    (globalThis as any).__smoothAudit = leaderAudit;

    // Start operator watchdog
    watchdog.start();

    const server = serve({
        fetch: app.fetch,
        port,
    });

    // Initialize WebSocket with heartbeat + event broadcasting
    initWebSocket(server);

    console.log(`Smooth leader running at http://localhost:${port}`);

    // Graceful shutdown
    const shutdown = async () => {
        console.log('Shutting down...');
        watchdog.stop();
        await flushAuditLogs();
        await shutdownBackend();
        process.exit(0);
    };
    process.on('SIGINT', shutdown);
    process.on('SIGTERM', shutdown);
}

start().catch((error) => {
    console.error('Failed to start leader:', error);
    process.exit(1);
});

export { app };

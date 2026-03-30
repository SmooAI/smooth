import { serve } from '@hono/node-server';
import { Hono } from 'hono';
import { cors } from 'hono/cors';
import { logger } from 'hono/logger';

import { beadsRoutes } from './routes/beads.js';
import { chatRoutes } from './routes/chat.js';
import { healthRoutes } from './routes/health.js';
import { jiraRoutes } from './routes/jira.js';
import { messagesRoutes } from './routes/messages.js';
import { projectsRoutes } from './routes/projects.js';
import { reviewsRoutes } from './routes/reviews.js';
import { streamRoutes } from './routes/stream.js';
import { systemRoutes } from './routes/system.js';
import { workersRoutes } from './routes/workers.js';

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

const port = parseInt(process.env.PORT ?? '4400', 10);

console.log(`Smooth leader starting on port ${port}...`);

serve({
    fetch: app.fetch,
    port,
});

console.log(`Smooth leader running at http://localhost:${port}`);

export { app };

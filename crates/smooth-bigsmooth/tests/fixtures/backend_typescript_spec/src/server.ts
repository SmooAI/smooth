import { Hono } from 'hono';

const VERSION = '0.1.0';

interface Task {
    id: string;
    title: string;
    description?: string;
    priority: string;
    status: string;
    tags: string[];
    created_at: string;
}

function newId(): string {
    // Simple UUID v4 using crypto
    const bytes = new Uint8Array(16);
    crypto.getRandomValues(bytes);
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    const hex = Array.from(bytes).map(b => b.toString(16).padStart(2, '0')).join('');
    return `${hex.slice(0,8)}-${hex.slice(8,12)}-${hex.slice(12,16)}-${hex.slice(16,20)}-${hex.slice(20)}`;
}

export function app(): Hono {
    const hono = new Hono();
    const db = new Map<string, Task>();

    hono.get('/health', (c) => {
        return c.json({ status: 'ok', version: VERSION });
    });

    hono.post('/tasks', async (c) => {
        let body: any;
        try {
            body = await c.req.json();
        } catch {
            return c.json({ error: 'invalid json' }, 400);
        }

        if (!body.title || typeof body.title !== 'string' || body.title.trim() === '') {
            return c.json({ error: 'title is required' }, 422);
        }

        const task: Task = {
            id: newId(),
            title: body.title,
            description: typeof body.description === 'string' ? body.description : undefined,
            priority: typeof body.priority === 'string' ? body.priority : 'medium',
            status: 'open',
            tags: Array.isArray(body.tags) ? body.tags.filter((t: any) => typeof t === 'string') : [],
            created_at: new Date().toISOString(),
        };

        db.set(task.id, task);
        return c.json(task, 201);
    });

    hono.get('/tasks', (c) => {
        const statusFilter = c.req.query('status');
        const priorityFilter = c.req.query('priority');
        let tasks = Array.from(db.values());
        if (statusFilter) tasks = tasks.filter(t => t.status === statusFilter);
        if (priorityFilter) tasks = tasks.filter(t => t.priority === priorityFilter);
        return c.json(tasks);
    });

    hono.get('/tasks/:id', (c) => {
        const id = c.req.param('id');
        const task = db.get(id);
        if (!task) return c.json({ error: 'not found' }, 404);
        return c.json(task);
    });

    hono.patch('/tasks/:id', async (c) => {
        const id = c.req.param('id');
        const task = db.get(id);
        if (!task) return c.json({ error: 'not found' }, 404);

        let body: any;
        try {
            body = await c.req.json();
        } catch {
            return c.json({ error: 'invalid json' }, 400);
        }

        if (typeof body.title === 'string') task.title = body.title;
        if (typeof body.description === 'string') task.description = body.description;
        if (typeof body.priority === 'string') task.priority = body.priority;
        if (typeof body.status === 'string') task.status = body.status;
        if (Array.isArray(body.tags)) task.tags = body.tags.filter((t: any) => typeof t === 'string');

        return c.json(task);
    });

    hono.delete('/tasks/:id', (c) => {
        const id = c.req.param('id');
        if (!db.has(id)) return c.json({ error: 'not found' }, 404);
        db.delete(id);
        return new Response(null, { status: 204 });
    });

    return hono;
}

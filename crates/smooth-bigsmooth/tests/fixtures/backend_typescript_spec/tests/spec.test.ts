// Contract tests for a small Hono task API.
//
// The agent is expected to write `src/server.ts` that exports
// `export function app(): Hono` implementing every endpoint the tests
// exercise. The tests drive the app via `app.fetch(request)` — no
// ephemeral port, no network — so they are fast and deterministic.
//
// This is the TypeScript mirror of `task_api_spec/tests/spec_test.rs`.

import { describe, it, expect, beforeEach } from 'vitest';
import { app } from '../src/server';

// Helper: build a Request and hand it to the Hono app. Returns (status, json body).
async function req(
    method: string,
    path: string,
    body?: unknown,
): Promise<{ status: number; body: any }> {
    const init: RequestInit = { method };
    if (body !== undefined) {
        init.body = JSON.stringify(body);
        init.headers = { 'content-type': 'application/json' };
    }
    const res = await app().fetch(new Request(`http://localhost${path}`, init));
    const text = await res.text();
    let parsed: any = null;
    if (text.length > 0) {
        try {
            parsed = JSON.parse(text);
        } catch {
            parsed = text;
        }
    }
    return { status: res.status, body: parsed };
}

describe('GET /health', () => {
    it('returns status ok and a version string', async () => {
        const { status, body } = await req('GET', '/health');
        expect(status).toBe(200);
        expect(body).toHaveProperty('status', 'ok');
        expect(typeof body.version).toBe('string');
    });
});

describe('POST /tasks', () => {
    it('creates a task with only a title and returns 201', async () => {
        const { status, body } = await req('POST', '/tasks', { title: 'Buy milk' });
        expect(status).toBe(201);
        expect(body).toHaveProperty('id');
        expect(body.title).toBe('Buy milk');
        expect(body.status).toBe('open');
        expect(body.priority).toBe('medium');
        expect(Array.isArray(body.tags)).toBe(true);
        expect(body.tags.length).toBe(0);
        expect(typeof body.created_at).toBe('string');
    });

    it('creates a task with all optional fields', async () => {
        const { status, body } = await req('POST', '/tasks', {
            title: 'Ship feature',
            description: 'Ship the Boardroom refactor',
            priority: 'high',
            tags: ['backend', 'urgent'],
        });
        expect(status).toBe(201);
        expect(body.title).toBe('Ship feature');
        expect(body.description).toBe('Ship the Boardroom refactor');
        expect(body.priority).toBe('high');
        expect(body.tags).toEqual(['backend', 'urgent']);
    });

    it('returns 400 or 422 when title is missing', async () => {
        const { status } = await req('POST', '/tasks', { description: 'no title here' });
        expect([400, 422]).toContain(status);
    });
});

describe('GET /tasks', () => {
    it('returns all tasks as a list', async () => {
        await req('POST', '/tasks', { title: 'one' });
        await req('POST', '/tasks', { title: 'two', priority: 'high' });
        const { status, body } = await req('GET', '/tasks');
        expect(status).toBe(200);
        expect(Array.isArray(body)).toBe(true);
        expect(body.length).toBeGreaterThanOrEqual(2);
    });

    it('filters by status query param', async () => {
        await req('POST', '/tasks', { title: 'filterable' });
        const { status, body } = await req('GET', '/tasks?status=open');
        expect(status).toBe(200);
        expect(Array.isArray(body)).toBe(true);
        for (const task of body) {
            expect(task.status).toBe('open');
        }
    });

    it('filters by priority query param', async () => {
        await req('POST', '/tasks', { title: 'lo', priority: 'low' });
        await req('POST', '/tasks', { title: 'hi', priority: 'high' });
        const { status, body } = await req('GET', '/tasks?priority=high');
        expect(status).toBe(200);
        for (const task of body) {
            expect(task.priority).toBe('high');
        }
    });
});

describe('GET /tasks/:id', () => {
    it('returns the task by id', async () => {
        const created = await req('POST', '/tasks', { title: 'findable' });
        const id = created.body.id;
        const { status, body } = await req('GET', `/tasks/${id}`);
        expect(status).toBe(200);
        expect(body.id).toBe(id);
        expect(body.title).toBe('findable');
    });

    it('returns 404 for unknown id', async () => {
        const { status } = await req('GET', '/tasks/does-not-exist');
        expect(status).toBe(404);
    });
});

describe('PATCH /tasks/:id', () => {
    it('updates status', async () => {
        const created = await req('POST', '/tasks', { title: 'updatable' });
        const { status, body } = await req('PATCH', `/tasks/${created.body.id}`, { status: 'in_progress' });
        expect(status).toBe(200);
        expect(body.status).toBe('in_progress');
    });

    it('updates title and priority together', async () => {
        const created = await req('POST', '/tasks', { title: 'old title' });
        const { status, body } = await req('PATCH', `/tasks/${created.body.id}`, { title: 'new title', priority: 'high' });
        expect(status).toBe(200);
        expect(body.title).toBe('new title');
        expect(body.priority).toBe('high');
    });
});

describe('DELETE /tasks/:id', () => {
    it('deletes a task and returns 204', async () => {
        const created = await req('POST', '/tasks', { title: 'doomed' });
        const del = await req('DELETE', `/tasks/${created.body.id}`);
        expect(del.status).toBe(204);
        const check = await req('GET', `/tasks/${created.body.id}`);
        expect(check.status).toBe(404);
    });
});

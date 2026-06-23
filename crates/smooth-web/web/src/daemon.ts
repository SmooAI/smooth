// Typed client for the smooth-daemon API (EPIC th-c89c2a / th-bd0def).
//
// Mirrors the daemon's wire protocol (crates/smooth-daemon/src/wire.rs).
// Used by the control surface (control.tsx); kept separate from the legacy
// Big Smooth api.ts so the two surfaces don't entangle.

export interface Health {
    service: string;
    version: string;
    status: string;
}

// GET /api/status — live daemon runtime state.
export interface Status {
    service: string;
    version: string;
    permission_mode: string;
    active_tasks: number;
    /** Egress-proxy address when the egress boundary is on, else null. */
    egress_proxy: string | null;
    /** Seconds since the daemon process started. */
    uptime_seconds: number;
}

export type SessionStatus = 'active' | 'idle' | 'completed';

export interface Session {
    id: string;
    title: string | null;
    created_at: string;
    updated_at: string;
    status: SessionStatus;
}

// Server → client events (#[serde(tag = "type")]).
export type ServerEvent =
    | { type: 'Connected'; session_id: string }
    | { type: 'Pong' }
    | { type: 'Error'; message: string }
    | { type: 'TokenDelta'; task_id: string; content: string }
    | { type: 'LlmIteration'; task_id: string; iteration: number }
    | { type: 'ToolCallStart'; task_id: string; tool_name: string; arguments: string }
    | { type: 'ToolCallComplete'; task_id: string; tool_name: string; result: string; is_error: boolean; duration_ms: number }
    | { type: 'TaskComplete'; task_id: string; iterations: number; cost_usd: number }
    | { type: 'TaskError'; task_id: string; message: string }
    | { type: 'PermissionRequest'; request_id: string; tool_name: string; summary: string };

// Client → server events.
export type ClientEvent =
    | { type: 'TaskStart'; message: string; model?: string; budget?: number; working_dir?: string }
    | { type: 'TaskCancel'; task_id: string }
    | { type: 'PermissionReply'; request_id: string; allow: boolean }
    | { type: 'Ping' };

export async function getHealth(): Promise<Health> {
    const r = await fetch('/health');
    if (!r.ok) throw new Error(`health ${r.status}`);
    return (await r.json()) as Health;
}

export async function getStatus(): Promise<Status> {
    const r = await fetch('/api/status');
    if (!r.ok) throw new Error(`status ${r.status}`);
    return (await r.json()) as Status;
}

// The Gate-1 permission postures the daemon understands (PUT /api/mode).
export const PERMISSION_MODES = ['default', 'acceptEdits', 'plan', 'auto', 'dontAsk', 'bypassPermissions'] as const;
export type PermissionMode = (typeof PERMISSION_MODES)[number];

/** Switch the daemon's runtime permission posture; resolves to the new mode. */
export async function setMode(mode: PermissionMode): Promise<string> {
    const r = await fetch('/api/mode', {
        method: 'PUT',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ mode }),
    });
    if (!r.ok) throw new Error(`set mode ${r.status}`);
    return ((await r.json()) as { permission_mode: string }).permission_mode;
}

// GET /api/memory — a recalled durable memory.
export interface MemoryHit {
    content: string;
    memory_type: string;
    relevance: number;
    created_at: string;
}

/** Search the agent's durable memory (keyword recall). Empty query → []. */
export async function searchMemory(query: string, limit = 50): Promise<MemoryHit[]> {
    const r = await fetch(`/api/memory?q=${encodeURIComponent(query)}&limit=${limit}`);
    if (!r.ok) throw new Error(`memory ${r.status}`);
    return (await r.json()) as MemoryHit[];
}

// GET/POST/DELETE /api/schedule — proactive scheduled tasks.
export type ScheduleKind = { kind: 'every_n_seconds'; secs: number } | { kind: 'daily_at'; hour: number; minute: number };

export interface Schedule {
    id: string;
    prompt: string;
    kind: ScheduleKind;
    enabled: boolean;
    next_due: string;
    last_run: string | null;
}

export async function listSchedules(): Promise<Schedule[]> {
    const r = await fetch('/api/schedule');
    if (!r.ok) throw new Error(`schedules ${r.status}`);
    return (await r.json()) as Schedule[];
}

export async function createSchedule(prompt: string, kind: ScheduleKind): Promise<Schedule> {
    const r = await fetch('/api/schedule', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ prompt, schedule: kind }),
    });
    if (!r.ok) throw new Error(`create schedule ${r.status}`);
    return (await r.json()) as Schedule;
}

export async function deleteSchedule(id: string): Promise<void> {
    const r = await fetch(`/api/schedule/${encodeURIComponent(id)}`, { method: 'DELETE' });
    if (!r.ok) throw new Error(`delete schedule ${r.status}`);
}

/** Parse a compact cadence string: `30m` (every N min) or `08:00` (daily UTC). */
export function parseCadence(input: string): ScheduleKind | null {
    const s = input.trim();
    const daily = /^(\d{1,2}):(\d{2})$/.exec(s);
    if (daily) {
        const hour = Number(daily[1]);
        const minute = Number(daily[2]);
        if (hour <= 23 && minute <= 59) return { kind: 'daily_at', hour, minute };
        return null;
    }
    const mins = /^(\d+)\s*m$/.exec(s);
    if (mins) {
        const n = Number(mins[1]);
        if (n >= 1) return { kind: 'every_n_seconds', secs: n * 60 };
    }
    return null;
}

export async function listSessions(): Promise<Session[]> {
    const r = await fetch('/api/session');
    if (!r.ok) throw new Error(`sessions ${r.status}`);
    return (await r.json()) as Session[];
}

export interface StoredMessage {
    role: string;
    content: string;
}

export async function listMessages(sessionId: string): Promise<StoredMessage[]> {
    const r = await fetch(`/api/session/${encodeURIComponent(sessionId)}/messages`);
    if (!r.ok) throw new Error(`messages ${r.status}`);
    return (await r.json()) as StoredMessage[];
}

export async function createSession(title?: string): Promise<Session> {
    const r = await fetch('/api/session', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ title: title ?? null }),
    });
    if (!r.ok) throw new Error(`create session ${r.status}`);
    return (await r.json()) as Session;
}

/** A resilient-ish WebSocket wrapper to the daemon's `/ws`. */
export class DaemonSocket {
    private ws: WebSocket | null = null;

    constructor(
        private readonly onEvent: (ev: ServerEvent) => void,
        private readonly onStatus: (connected: boolean) => void,
        private readonly resumeSession?: string,
    ) {}

    connect(): void {
        const proto = location.protocol === 'https:' ? 'wss' : 'ws';
        const q = this.resumeSession ? `?session=${encodeURIComponent(this.resumeSession)}` : '';
        const ws = new WebSocket(`${proto}://${location.host}/ws${q}`);
        this.ws = ws;
        ws.onopen = () => this.onStatus(true);
        ws.onclose = () => {
            this.onStatus(false);
            // Reconnect after a short delay (the daemon is always-on).
            setTimeout(() => this.connect(), 1000);
        };
        ws.onmessage = (e) => {
            if (typeof e.data !== 'string') return;
            try {
                this.onEvent(JSON.parse(e.data) as ServerEvent);
            } catch {
                /* ignore malformed frames */
            }
        };
    }

    send(ev: ClientEvent): void {
        if (this.ws && this.ws.readyState === WebSocket.OPEN) {
            this.ws.send(JSON.stringify(ev));
        }
    }

    close(): void {
        if (this.ws) {
            this.ws.onclose = null; // disable reconnect
            this.ws.close();
        }
    }
}

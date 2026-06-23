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

/** WebSocket route — resilient real-time event stream
 *
 * Protocol:
 *   Server → Client:
 *     { type: "event", event: ExecutionEvent }     — execution events
 *     { type: "heartbeat", ts: number }            — keepalive (every 15s)
 *     { type: "welcome", clientId: string }        — connection established
 *     { type: "error", message: string }           — error notification
 *
 *   Client → Server:
 *     { type: "ping" }                             — client heartbeat
 *     { type: "subscribe", topics: string[] }      — filter events (optional)
 *     { type: "steering", beadId, action, message? } — steering commands
 *
 * Resilience:
 *   - Server sends heartbeat every 15s; client should reconnect if 3 missed (45s)
 *   - Each client gets a unique ID for tracking
 *   - Missed events during disconnect are not replayed (use REST API for state)
 *   - Server tracks connected clients and cleans up dead connections
 */

import { createNodeWebSocket } from '@hono/node-ws';
import type { ServerType } from '@hono/node-server';
import { Hono } from 'hono';

import { createAuditLogger } from '@smooai/smooth-shared/audit-log';

import type { ExecutionEvent } from '../backend/types.js';
import { getBackend } from '../backend/registry.js';
import { getEventStream } from '../backend/registry.js';
import { updateBead } from '../beads/client.js';
import { sendMessage } from '../beads/messaging.js';

const audit = createAuditLogger('leader');

const HEARTBEAT_INTERVAL_MS = 15_000;
const CLIENT_TIMEOUT_MS = 45_000;

interface ConnectedClient {
    id: string;
    ws: WebSocket;
    subscribedTopics: Set<string>;
    lastPong: number;
    connectedAt: number;
}

const clients = new Map<string, ConnectedClient>();
let clientCounter = 0;

/** Broadcast an event to all connected clients (filtered by subscriptions) */
function broadcast(event: ExecutionEvent): void {
    const message = JSON.stringify({ type: 'event', event });

    for (const [id, client] of clients) {
        try {
            // If client has topic filters, check if this event matches
            if (client.subscribedTopics.size > 0) {
                const matches =
                    client.subscribedTopics.has(event.type) ||
                    client.subscribedTopics.has('*') ||
                    (event.beadId && client.subscribedTopics.has(`bead:${event.beadId}`)) ||
                    client.subscribedTopics.has(`sandbox:${event.sandboxId}`);
                if (!matches) continue;
            }

            if (client.ws.readyState === WebSocket.OPEN) {
                client.ws.send(message);
            }
        } catch {
            // Dead connection — will be cleaned up by heartbeat
            clients.delete(id);
        }
    }
}

/** Send heartbeats and clean up dead clients */
function heartbeatTick(): void {
    const now = Date.now();
    const heartbeatMsg = JSON.stringify({ type: 'heartbeat', ts: now });

    for (const [id, client] of clients) {
        // Clean up clients that haven't responded
        if (now - client.lastPong > CLIENT_TIMEOUT_MS) {
            console.log(`[ws] Client ${id} timed out (no pong for ${CLIENT_TIMEOUT_MS / 1000}s)`);
            try {
                client.ws.close(1000, 'Timeout');
            } catch {
                /* already closed */
            }
            clients.delete(id);
            continue;
        }

        // Send heartbeat
        try {
            if (client.ws.readyState === WebSocket.OPEN) {
                client.ws.send(heartbeatMsg);
            }
        } catch {
            clients.delete(id);
        }
    }
}

/** Handle incoming client messages */
async function handleClientMessage(clientId: string, data: string): Promise<void> {
    let msg: any;
    try {
        msg = JSON.parse(data);
    } catch {
        return;
    }

    const client = clients.get(clientId);
    if (!client) return;

    switch (msg.type) {
        case 'ping':
            client.lastPong = Date.now();
            try {
                client.ws.send(JSON.stringify({ type: 'pong', ts: Date.now() }));
            } catch {
                /* ignore */
            }
            break;

        case 'subscribe':
            if (Array.isArray(msg.topics)) {
                client.subscribedTopics = new Set(msg.topics);
            }
            break;

        case 'steering': {
            const { beadId, action, message } = msg;
            if (!beadId || !action) break;

            try {
                switch (action) {
                    case 'pause':
                        await sendMessage(beadId, 'leader→worker', '[STEERING:PAUSE] Operator paused by human.', 'leader');
                        await updateBead(beadId, { addLabel: 'steering:paused' });
                        break;
                    case 'resume':
                        await sendMessage(beadId, 'leader→worker', '[STEERING:RESUME] Operator resumed.', 'leader');
                        await updateBead(beadId, { removeLabel: 'steering:paused' });
                        break;
                    case 'steer':
                        if (message) await sendMessage(beadId, 'leader→worker', `[STEERING:GUIDANCE] ${message}`, 'leader');
                        break;
                    case 'cancel':
                        await sendMessage(beadId, 'leader→worker', '[STEERING:CANCEL] Operator cancelled.', 'leader');
                        await updateBead(beadId, { addLabel: 'steering:cancelled' });
                        const backend = getBackend();
                        const sandboxes = await backend.listSandboxes();
                        const sandbox = sandboxes.find((s) => s.beadId === beadId);
                        if (sandbox) await backend.destroySandbox(sandbox.sandboxId);
                        break;
                }
                client.ws.send(JSON.stringify({ type: 'steering_ack', beadId, action, ok: true }));
            } catch (error) {
                client.ws.send(JSON.stringify({ type: 'steering_ack', beadId, action, ok: false, error: (error as Error).message }));
            }
            break;
        }
    }
}

// ── Hono WebSocket setup ────────────────────────────────

export const wsApp = new Hono();
const { upgradeWebSocket, injectWebSocket } = createNodeWebSocket({ app: wsApp });

wsApp.get(
    '/ws',
    upgradeWebSocket(() => {
        const clientId = `client-${++clientCounter}-${Date.now().toString(36)}`;

        return {
            onOpen(_evt, ws) {
                const rawWs = ws.raw as WebSocket;
                clients.set(clientId, {
                    id: clientId,
                    ws: rawWs,
                    subscribedTopics: new Set(),
                    lastPong: Date.now(),
                    connectedAt: Date.now(),
                });

                rawWs.send(JSON.stringify({ type: 'welcome', clientId, connectedClients: clients.size }));
                audit.messageSent('system', 'ws', `Client ${clientId} connected (total: ${clients.size})`);
            },

            onMessage(evt, _ws) {
                const data = typeof evt.data === 'string' ? evt.data : evt.data.toString();
                handleClientMessage(clientId, data).catch(() => {});
            },

            onClose() {
                clients.delete(clientId);
                audit.messageSent('system', 'ws', `Client ${clientId} disconnected (total: ${clients.size})`);
            },

            onError() {
                clients.delete(clientId);
            },
        };
    }),
);

/** Initialize WebSocket: wire EventStream → broadcast, start heartbeat */
export function initWebSocket(server: ServerType): void {
    injectWebSocket(server);

    // Wire execution events to WebSocket broadcast
    const events = getEventStream();
    events.on('*', (event) => {
        broadcast(event);
    });

    // Start heartbeat
    setInterval(heartbeatTick, HEARTBEAT_INTERVAL_MS);

    console.log(`[ws] WebSocket ready at /ws (heartbeat: ${HEARTBEAT_INTERVAL_MS / 1000}s)`);
}

/** Get connected client count */
export function getConnectedClients(): number {
    return clients.size;
}

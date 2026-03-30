/** Resilient WebSocket hook for Next.js
 *
 * Auto-reconnects on disconnect with exponential backoff.
 * Monitors heartbeats — reconnects if 3 missed (45s).
 * Exposes send() for steering commands.
 */

'use client';

import { useCallback, useEffect, useRef, useState } from 'react';

const WS_URL = typeof window !== 'undefined' ? `ws://${window.location.hostname}:4400/ws` : '';
const HEARTBEAT_TIMEOUT_MS = 45_000;
const MAX_RECONNECT_DELAY_MS = 30_000;
const INITIAL_RECONNECT_DELAY_MS = 1_000;

export type WsStatus = 'connecting' | 'connected' | 'disconnected' | 'reconnecting';

export interface WsEvent {
    type: string;
    event?: any;
    ts?: number;
    clientId?: string;
    [key: string]: any;
}

export function useWebSocket(options?: { topics?: string[]; onEvent?: (event: WsEvent) => void }) {
    const [status, setStatus] = useState<WsStatus>('disconnected');
    const [clientId, setClientId] = useState<string | null>(null);
    const [lastEvent, setLastEvent] = useState<WsEvent | null>(null);
    const wsRef = useRef<WebSocket | null>(null);
    const reconnectDelay = useRef(INITIAL_RECONNECT_DELAY_MS);
    const heartbeatTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
    const reconnectTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
    const mountedRef = useRef(true);
    const optionsRef = useRef(options);
    optionsRef.current = options;

    const resetHeartbeatTimer = useCallback(() => {
        if (heartbeatTimer.current) clearTimeout(heartbeatTimer.current);
        heartbeatTimer.current = setTimeout(() => {
            // No heartbeat received — connection is dead
            console.log('[ws] Heartbeat timeout, reconnecting...');
            wsRef.current?.close();
        }, HEARTBEAT_TIMEOUT_MS);
    }, []);

    const connect = useCallback(() => {
        if (!mountedRef.current || !WS_URL) return;

        setStatus('connecting');
        const ws = new WebSocket(WS_URL);
        wsRef.current = ws;

        ws.onopen = () => {
            if (!mountedRef.current) return;
            setStatus('connected');
            reconnectDelay.current = INITIAL_RECONNECT_DELAY_MS;
            resetHeartbeatTimer();

            // Subscribe to topics if specified
            if (optionsRef.current?.topics?.length) {
                ws.send(JSON.stringify({ type: 'subscribe', topics: optionsRef.current.topics }));
            }
        };

        ws.onmessage = (evt) => {
            if (!mountedRef.current) return;

            let msg: WsEvent;
            try {
                msg = JSON.parse(evt.data);
            } catch {
                return;
            }

            // Reset heartbeat on any message from server
            resetHeartbeatTimer();

            switch (msg.type) {
                case 'welcome':
                    setClientId(msg.clientId ?? null);
                    break;
                case 'heartbeat':
                    // Send pong back
                    ws.send(JSON.stringify({ type: 'ping' }));
                    break;
                case 'pong':
                    // Server acknowledged our ping
                    break;
                case 'event':
                    setLastEvent(msg);
                    optionsRef.current?.onEvent?.(msg);
                    break;
                default:
                    setLastEvent(msg);
                    optionsRef.current?.onEvent?.(msg);
            }
        };

        ws.onclose = () => {
            if (!mountedRef.current) return;
            setStatus('reconnecting');
            if (heartbeatTimer.current) clearTimeout(heartbeatTimer.current);

            // Exponential backoff reconnect
            const delay = reconnectDelay.current;
            reconnectDelay.current = Math.min(delay * 2, MAX_RECONNECT_DELAY_MS);
            console.log(`[ws] Disconnected, reconnecting in ${delay}ms...`);

            reconnectTimer.current = setTimeout(connect, delay);
        };

        ws.onerror = () => {
            // onclose will fire after this
        };
    }, [resetHeartbeatTimer]);

    useEffect(() => {
        mountedRef.current = true;
        connect();

        return () => {
            mountedRef.current = false;
            if (heartbeatTimer.current) clearTimeout(heartbeatTimer.current);
            if (reconnectTimer.current) clearTimeout(reconnectTimer.current);
            wsRef.current?.close();
        };
    }, [connect]);

    /** Send a message to the leader via WebSocket */
    const send = useCallback((msg: Record<string, unknown>) => {
        if (wsRef.current?.readyState === WebSocket.OPEN) {
            wsRef.current.send(JSON.stringify(msg));
        }
    }, []);

    /** Send a steering command */
    const steer = useCallback(
        (beadId: string, action: 'pause' | 'resume' | 'steer' | 'cancel', message?: string) => {
            send({ type: 'steering', beadId, action, message });
        },
        [send],
    );

    return { status, clientId, lastEvent, send, steer };
}

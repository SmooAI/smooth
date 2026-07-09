// operator.ts — a React hook that speaks the smooth-operator canonical WS
// protocol (the same one `th code` / the official widget use), so smooth-web is
// a thin client on the one protocol. EPIC th-c89c2a (th-f1a1f0).
//
// It owns a single WS connection + one conversation session, derives the agent's
// live "presence" state, accumulates the streaming conversation, and surfaces
// pending write-tool approvals (the HITL the control surface makes its hero).

import { useCallback, useEffect, useMemo, useRef, useState } from 'react';

import { DEFAULT_MODE_ID, modeById, type ModelCosts, type SmoothMode } from './modes';

/** The agent's live presence — what the face reflects. */
export type AgentState = 'connecting' | 'offline' | 'awake' | 'thinking' | 'speaking' | 'awaiting';

export interface ToolCall {
    id: string;
    name: string;
    args: string;
    result?: string;
    isError?: boolean;
    done: boolean;
}

/** A pending/sent attachment. `dataUrl` is the full `data:<mime>;base64,…`
 * string — it's both the wire value (the backend's `images[]`) and the preview. */
export interface Attachment {
    name: string;
    mime: string;
    dataUrl: string;
}

/** One ordered segment of an assistant turn: a run of prose, or a tool call.
 * `blocks` preserves the interleave order the model produced (say a bit → call a
 * tool → say a bit → …) so the UI shows tools INLINE between text instead of
 * piling every chip at the top and concatenating all prose at the bottom. */
export type MessageBlock = { kind: 'text'; text: string } | { kind: 'tool'; tool: ToolCall };

export interface ChatMessage {
    id: string;
    role: 'user' | 'assistant' | 'system';
    content: string;
    /** Reasoning-model "thinking" — captured on its own channel and shown
     * collapsed, never folded into `content` (the answer). */
    reasoning: string;
    tools: ToolCall[];
    /** Text + tool segments in the order the model streamed them (assistant turns). */
    blocks: MessageBlock[];
    streaming: boolean;
    /** Images/PDFs sent with a user message. */
    attachments?: Attachment[];
}

/** A parked write-tool the agent needs a human verdict on. */
export interface Approval {
    requestId: string;
    tool: string;
    description: string;
}

/** One row in the conversation sidebar — from `list_conversations`. */
export interface ConversationSummary {
    conversationId: string;
    title: string;
    /** Server-provided; ISO string or epoch. Rendered defensively (see App relTime). */
    updatedAt: string | number;
    messageCount: number;
}

/** A raw history message from `get_conversation_messages`. Shape is assumed
 * `{ role, content }` (defensive on `text`/`message` aliases) — reconcile with
 * the server agent's actual payload. Past tool calls are NOT reconstructed; only
 * the text of each turn is rendered. */
interface HistoryMessage {
    role?: string;
    content?: string;
    text?: string;
    message?: string;
}

/** Render server history into our ChatMessage model: user turns are inbound,
 * everything else assistant/system. Assistant text becomes a single text block. */
function renderHistory(raw: HistoryMessage[]): ChatMessage[] {
    return (raw ?? []).map((m) => {
        const content = m.content ?? m.text ?? m.message ?? '';
        const role: ChatMessage['role'] = m.role === 'user' ? 'user' : m.role === 'system' ? 'system' : 'assistant';
        return {
            id: nextId('h'),
            role,
            content,
            reasoning: '',
            tools: [],
            blocks: role === 'assistant' ? [{ kind: 'text', text: content }] : [],
            streaming: false,
        };
    });
}

export interface Status {
    connected: boolean;
    model?: string;
    identity?: string;
    since: number;
}

interface OperatorApi {
    state: AgentState;
    messages: ChatMessage[];
    approvals: Approval[];
    status: Status;
    sendMessage: (text: string, attachments?: Attachment[]) => void;
    respond: (requestId: string, approved: boolean) => void;
    /** Recent conversations for the sidebar (most-recent first), from `list_conversations`. */
    conversations: ConversationSummary[];
    /** The conversation currently loaded, for highlighting the active sidebar row. */
    activeConversationId: string | null;
    /** Re-request `list_conversations` (called after a turn completes). */
    refreshConversations: () => void;
    /** Bind a session to an existing conversation and load its history into `messages`. */
    resumeConversation: (conversationId: string) => void;
    /** Start a fresh conversation (new session, cleared transcript). */
    newConversation: () => void;
    /** The active Smooth Mode — pins each turn to a model. */
    mode: SmoothMode;
    /** Switch modes by id; persisted to localStorage `smooth.mode`. */
    setMode: (id: string) => void;
    /** Running USD spend this session, summed from each turn's `costUsd`. */
    sessionCostUsd: number;
    /** Per-model cost table from `GET /admin/model-costs` (empty on failure). */
    modelCosts: ModelCosts;
}

/** Globals the daemon injects into `index.html` when it serves this SPA
 * same-origin (see smooth-web's `web_router_with_token`): the auth token (and,
 * optionally, an explicit API base). These take priority over the `?api`/`?token`
 * dev query params so the same-origin endpoint is just `http://127.0.0.1:8787/`. */
declare global {
    interface Window {
        __SMOOTH_TOKEN__?: string;
        __SMOOTH_API__?: string;
    }
}

/** Resolve the operator's HTTP base + auth token. When the daemon serves this
 * SPA same-origin it injects `window.__SMOOTH_TOKEN__` (highest priority), so the
 * endpoint is simply `http://127.0.0.1:8787/`. In dev (Vite at :3100) pass them
 * as `?api=http://127.0.0.1:8787&token=…` (persisted to localStorage thereafter);
 * the API base otherwise defaults to the page origin. */
export function resolveTarget(): { http: string; token: string } {
    const params = new URLSearchParams(window.location.search);
    const api = window.__SMOOTH_API__ ?? params.get('api') ?? localStorage.getItem('smooth.api') ?? window.location.origin;
    const token = window.__SMOOTH_TOKEN__ ?? params.get('token') ?? localStorage.getItem('smooth.token') ?? '';
    if (params.get('api')) localStorage.setItem('smooth.api', api);
    if (params.get('token')) localStorage.setItem('smooth.token', token);
    return { http: api.replace(/\/$/, ''), token };
}

let msgSeq = 0;
const nextId = (p: string) => `${p}-${++msgSeq}`;

export function useOperator(): OperatorApi {
    const [messages, setMessages] = useState<ChatMessage[]>([]);
    const [approvals, setApprovals] = useState<Approval[]>([]);
    const [connected, setConnected] = useState(false);
    const [turnActive, setTurnActive] = useState(false);
    const [streaming, setStreaming] = useState(false);
    const [status, setStatus] = useState<Status>({ connected: false, since: Date.now() });
    const [modeId, setModeId] = useState<string>(() => localStorage.getItem('smooth.mode') ?? DEFAULT_MODE_ID);
    const [sessionCostUsd, setSessionCostUsd] = useState(0);
    const [modelCosts, setModelCosts] = useState<ModelCosts>({});
    const [conversations, setConversations] = useState<ConversationSummary[]>([]);
    const [activeConversationId, setActiveConversationId] = useState<string | null>(null);
    // When set, the create_conversation_session reply should trigger a history
    // load for this conversationId (resume can't fetch messages until it has a
    // sessionId from the create reply).
    const pendingResumeRef = useRef<string | null>(null);

    const mode = useMemo(() => modeById(modeId), [modeId]);
    // Keep a ref so `sendMessage` always reads the live model without re-binding.
    const modeRef = useRef(mode);
    modeRef.current = mode;

    const setMode = useCallback((id: string) => {
        const next = modeById(id);
        setModeId(next.id);
        localStorage.setItem('smooth.mode', next.id);
    }, []);

    const wsRef = useRef<WebSocket | null>(null);
    const sessionRef = useRef<string | null>(null);
    const targetRef = useRef(resolveTarget());
    const reconnectRef = useRef<ReturnType<typeof setTimeout> | null>(null);

    // Mutate the in-flight assistant message (the last streaming one).
    const patchStreaming = useCallback((fn: (m: ChatMessage) => ChatMessage) => {
        setMessages((prev) => {
            const i = [...prev].reverse().findIndex((m) => m.role === 'assistant' && m.streaming);
            if (i === -1) return prev;
            const idx = prev.length - 1 - i;
            const copy = prev.slice();
            copy[idx] = fn(copy[idx]);
            return copy;
        });
    }, []);

    const ensureStreamingMessage = useCallback(() => {
        setMessages((prev) => {
            const hasOpen = prev.some((m) => m.role === 'assistant' && m.streaming);
            if (hasOpen) return prev;
            return [...prev, { id: nextId('a'), role: 'assistant', content: '', reasoning: '', tools: [], blocks: [], streaming: true }];
        });
    }, []);

    const send = useCallback((obj: unknown) => {
        wsRef.current?.readyState === WebSocket.OPEN && wsRef.current.send(JSON.stringify(obj));
    }, []);

    const handle = useCallback(
        (v: any) => {
            switch (v?.type) {
                case 'immediate_response': {
                    const d = v?.data ?? {};
                    // list_conversations → the sidebar list (most-recent first, non-empty only).
                    if (Array.isArray(d.conversations)) {
                        setConversations(d.conversations as ConversationSummary[]);
                        break;
                    }
                    // get_conversation_messages → history for a resumed conversation.
                    if (Array.isArray(d.messages)) {
                        if (d.conversationId) setActiveConversationId(d.conversationId);
                        setMessages(renderHistory(d.messages as HistoryMessage[]));
                        break;
                    }
                    // create_conversation_session → bind the session; on a resume, chase
                    // it with a history load now that we have the sessionId.
                    if (d.sessionId) {
                        sessionRef.current = d.sessionId;
                        if (d.conversationId) setActiveConversationId(d.conversationId);
                        const resume = pendingResumeRef.current;
                        if (resume) {
                            pendingResumeRef.current = null;
                            send({ action: 'get_conversation_messages', requestId: nextId('gm'), sessionId: d.sessionId, conversationId: resume });
                        }
                    }
                    break;
                }
                case 'stream_token':
                    ensureStreamingMessage();
                    setStreaming(true);
                    patchStreaming((m) => {
                        const tok = v.token ?? '';
                        // Grow the trailing text block, or open a new one if the last
                        // block was a tool — that's what interleaves prose with chips.
                        const blocks = m.blocks.slice();
                        const last = blocks[blocks.length - 1];
                        if (last && last.kind === 'text') blocks[blocks.length - 1] = { kind: 'text', text: last.text + tok };
                        else blocks.push({ kind: 'text', text: tok });
                        return { ...m, content: m.content + tok, blocks };
                    });
                    break;
                case 'stream_reasoning':
                    // Reasoning rides its own channel — accumulate it separately
                    // so it shows as "thinking", never as the answer (th-4d8682).
                    // Don't flip `streaming` (that means "speaking the answer");
                    // reasoning-only keeps him in the `thinking` state.
                    ensureStreamingMessage();
                    patchStreaming((m) => ({ ...m, reasoning: m.reasoning + (v.token ?? '') }));
                    break;
                case 'stream_chunk': {
                    const st = v?.data?.state;
                    const call = st?.rawResponse?.toolCall;
                    // toolResult is nested under rawResponse, same as toolCall (server
                    // runner.rs). Reading st.toolResult left every chip stuck on
                    // "running…" forever because the completion branch never fired.
                    const res = st?.rawResponse?.toolResult;
                    if (call) {
                        ensureStreamingMessage();
                        const args = typeof call.arguments === 'string' ? call.arguments : JSON.stringify(call.arguments ?? {});
                        const tool: ToolCall = { id: nextId('t'), name: call.name ?? '', args, done: false };
                        // Push the tool as its own block so it renders inline, right
                        // where the model called it — between the prose around it.
                        patchStreaming((m) => ({ ...m, tools: [...m.tools, tool], blocks: [...m.blocks, { kind: 'tool', tool }] }));
                    } else if (res) {
                        patchStreaming((m) => {
                            const complete = (t: ToolCall): ToolCall => ({
                                ...t,
                                done: true,
                                isError: !!res.isError,
                                result: typeof res.result === 'string' ? res.result : JSON.stringify(res.result ?? ''),
                            });
                            // Complete the most recent open tool with this name, in both
                            // the flat list and the ordered blocks.
                            const tools = m.tools.slice();
                            for (let i = tools.length - 1; i >= 0; i--) {
                                if (tools[i].name === res.name && !tools[i].done) {
                                    tools[i] = complete(tools[i]);
                                    break;
                                }
                            }
                            const blocks = m.blocks.slice();
                            for (let i = blocks.length - 1; i >= 0; i--) {
                                const b = blocks[i];
                                if (b.kind === 'tool' && b.tool.name === res.name && !b.tool.done) {
                                    blocks[i] = { kind: 'tool', tool: complete(b.tool) };
                                    break;
                                }
                            }
                            return { ...m, tools, blocks };
                        });
                    }
                    break;
                }
                case 'write_confirmation_required': {
                    const d = v?.data?.data ?? {};
                    setApprovals((prev) => [
                        ...prev.filter((a) => a.requestId !== v.requestId),
                        { requestId: v.requestId, tool: d.toolId ?? 'tool', description: d.actionDescription ?? '' },
                    ]);
                    break;
                }
                case 'eventual_response': {
                    setTurnActive(false);
                    setStreaming(false);
                    patchStreaming((m) => ({ ...m, streaming: false }));
                    // Usage rides the eventual_response — read defensively since the
                    // exact path may shift slightly at integration (th-2a6330).
                    const cost = Number(v?.data?.data?.usage?.costUsd ?? v?.data?.data?.costUsd ?? v?.data?.costUsd ?? v?.usage?.costUsd);
                    if (Number.isFinite(cost) && cost > 0) setSessionCostUsd((c) => c + cost);
                    // The turn just landed — refresh the sidebar so this chat appears/updates.
                    send({ action: 'list_conversations', requestId: nextId('lc') });
                    break;
                }
                case 'error':
                    setTurnActive(false);
                    setStreaming(false);
                    patchStreaming((m) => ({ ...m, streaming: false }));
                    setMessages((prev) => [
                        ...prev,
                        {
                            id: nextId('e'),
                            role: 'system',
                            content: v.message ?? v?.data?.message ?? 'operator error',
                            reasoning: '',
                            tools: [],
                            blocks: [],
                            streaming: false,
                        },
                    ]);
                    break;
                default:
                    break;
            }
        },
        [ensureStreamingMessage, patchStreaming, send],
    );

    useEffect(() => {
        let closed = false;
        const connect = () => {
            const { http, token } = targetRef.current;
            const wsUrl = `${http.replace(/^http/, 'ws')}/ws${token ? `?token=${encodeURIComponent(token)}` : ''}`;
            const ws = new WebSocket(wsUrl);
            wsRef.current = ws;

            ws.onopen = () => {
                setConnected(true);
                setStatus((s) => ({ ...s, connected: true, since: Date.now() }));
                // Open one persistent session for the control surface.
                send({ action: 'create_conversation_session', requestId: nextId('cs'), agentId: crypto.randomUUID(), userName: 'console' });
                // Pull the conversation history list for the sidebar.
                send({ action: 'list_conversations', requestId: nextId('lc') });
                // Best-effort identity/health.
                fetch(`${http}/admin/me`, { headers: token ? { authorization: `Bearer ${token}` } : {} })
                    .then((r) => (r.ok ? r.json() : null))
                    .then((me) => me && setStatus((s) => ({ ...s, identity: me.name ?? me.id, model: me.model })))
                    .catch(() => {});
                // Per-model cost table — best-effort; the cost bar degrades gracefully when empty.
                fetch(`${http}/admin/model-costs`, { headers: token ? { authorization: `Bearer ${token}` } : {} })
                    .then((r) => (r.ok ? r.json() : null))
                    .then((costs) => costs && setModelCosts(costs as ModelCosts))
                    .catch(() => {});
            };
            ws.onmessage = (e) => {
                try {
                    handle(JSON.parse(e.data));
                } catch {
                    /* ignore non-JSON frames */
                }
            };
            ws.onclose = () => {
                setConnected(false);
                setStatus((s) => ({ ...s, connected: false }));
                if (!closed) reconnectRef.current = setTimeout(connect, 1500);
            };
            ws.onerror = () => ws.close();
        };
        connect();
        return () => {
            closed = true;
            if (reconnectRef.current) clearTimeout(reconnectRef.current);
            wsRef.current?.close();
        };
    }, [handle, send]);

    const sendMessage = useCallback(
        (text: string, attachments: Attachment[] = []) => {
            const body = text.trim();
            if ((!body && attachments.length === 0) || !sessionRef.current) return;
            setMessages((prev) => [
                ...prev,
                {
                    id: nextId('u'),
                    role: 'user',
                    content: body,
                    reasoning: '',
                    tools: [],
                    blocks: [],
                    streaming: false,
                    attachments: attachments.length ? attachments : undefined,
                },
            ]);
            setTurnActive(true);
            // The backend accepts an optional `images` array of full data-URL strings;
            // omit it entirely for text-only sends so nothing changes there.
            const frame: Record<string, unknown> = {
                action: 'send_message',
                requestId: nextId('turn'),
                sessionId: sessionRef.current,
                message: body,
                model: modeRef.current.model,
            };
            if (attachments.length) frame.images = attachments.map((a) => a.dataUrl);
            send(frame);
        },
        [send],
    );

    const respond = useCallback(
        (requestId: string, approved: boolean) => {
            setApprovals((prev) => prev.filter((a) => a.requestId !== requestId));
            send({ action: 'confirm_tool_action', requestId, sessionId: sessionRef.current, approved });
        },
        [send],
    );

    const refreshConversations = useCallback(() => {
        send({ action: 'list_conversations', requestId: nextId('lc') });
    }, [send]);

    // Resume: bind a NEW session to the existing conversationId, then load its
    // history. The get_conversation_messages fires from the create reply handler
    // (it needs the sessionId), driven by pendingResumeRef.
    const resumeConversation = useCallback(
        (conversationId: string) => {
            if (!conversationId) return;
            setActiveConversationId(conversationId);
            setMessages([]);
            setApprovals([]);
            pendingResumeRef.current = conversationId;
            sessionRef.current = null;
            send({ action: 'create_conversation_session', requestId: nextId('cs'), agentId: crypto.randomUUID(), conversationId, userName: 'console' });
        },
        [send],
    );

    // New chat: fresh session (no conversationId), cleared transcript.
    const newConversation = useCallback(() => {
        pendingResumeRef.current = null;
        setActiveConversationId(null);
        setMessages([]);
        setApprovals([]);
        sessionRef.current = null;
        send({ action: 'create_conversation_session', requestId: nextId('cs'), agentId: crypto.randomUUID(), userName: 'console' });
    }, [send]);

    const state: AgentState = useMemo(() => {
        if (!connected) return reconnectRef.current ? 'offline' : 'connecting';
        if (approvals.length) return 'awaiting';
        if (streaming) return 'speaking';
        if (turnActive) return 'thinking';
        return 'awake';
    }, [connected, approvals.length, streaming, turnActive]);

    return {
        state,
        messages,
        approvals,
        status,
        sendMessage,
        respond,
        mode,
        setMode,
        sessionCostUsd,
        modelCosts,
        conversations,
        activeConversationId,
        refreshConversations,
        resumeConversation,
        newConversation,
    };
}

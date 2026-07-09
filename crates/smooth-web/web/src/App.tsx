// The Presence control surface — Big Smooth as a character you cohabit with.
// His face is the room: he greets you large and centred, then settles into a
// persistent presence bar once you're talking. A halo breathes behind him and
// shifts colour with his state, so the "needs you" moment emanates from him
// rather than a detached banner. A thin client on the operator's canonical
// protocol (EPIC th-c89c2a, th-f1a1f0, th-833b5f).

import { ArrowUp, Check, X, Terminal, FileText, Search, Folder, Pencil, Brain, Bell, BellRing, Paperclip, Menu, Plus, MessageSquare } from 'lucide-react';
import { Component, useEffect, useMemo, useRef, useState, type ReactNode } from 'react';
import Markdown from 'react-markdown';
import remarkGfm from 'remark-gfm';

import { BigSmoothFace, type FaceState } from './components/BigSmoothFace';
import { MODES, costBadge, isExpensiveBadge, blendedPerMillion, type SmoothMode, type ModelCost, type ModelCosts } from './modes';
import {
    useOperator,
    type AgentState,
    type Attachment,
    type ChatMessage,
    type ToolCall,
    type Approval,
    type Status,
    type ConversationSummary,
} from './operator';
import { useMentionSearch, activeMention, type MentionResult } from './useMentionSearch';
import { usePush } from './usePush';

const STATUS_CAPTION: Record<AgentState, string> = {
    connecting: 'waking up',
    offline: 'offline — reconnecting',
    awake: 'awake',
    thinking: 'thinking',
    speaking: 'speaking',
    awaiting: 'needs your okay',
};

const TOOL_ICON: Record<string, typeof Terminal> = {
    bash: Terminal,
    read_file: FileText,
    write_file: Pencil,
    edit_file: Pencil,
    grep: Search,
    list_files: Folder,
};

// Downscale an image to <=MAX_IMAGE_DIM on its longest edge and re-encode as
// JPEG, returning a data-URL. A raw phone photo is several MB of base64, which
// overruns the LLM gateway (HTTP 524 timeout); vision models see no benefit past
// ~1568px. Pearl th-3be564.
const MAX_IMAGE_DIM = 1568;
function downscaleImage(file: File): Promise<string> {
    return new Promise((resolve, reject) => {
        const url = URL.createObjectURL(file);
        const img = new Image();
        img.onload = () => {
            URL.revokeObjectURL(url);
            const scale = Math.min(1, MAX_IMAGE_DIM / Math.max(img.width, img.height));
            const w = Math.max(1, Math.round(img.width * scale));
            const h = Math.max(1, Math.round(img.height * scale));
            const canvas = document.createElement('canvas');
            canvas.width = w;
            canvas.height = h;
            const ctx = canvas.getContext('2d');
            if (!ctx) {
                reject(new Error('no 2d context'));
                return;
            }
            ctx.drawImage(img, 0, 0, w, h);
            resolve(canvas.toDataURL('image/jpeg', 0.85));
        };
        img.onerror = () => {
            URL.revokeObjectURL(url);
            reject(new Error('image decode failed'));
        };
        img.src = url;
    });
}

function uptime(since: number): string {
    const s = Math.max(0, Math.floor((Date.now() - since) / 1000));
    if (s < 60) return `${s}s`;
    if (s < 3600) return `${Math.floor(s / 60)}m`;
    if (s < 86400) return `${Math.floor(s / 3600)}h`;
    return `${Math.floor(s / 86400)}d`;
}

/** "3m ago" for a sidebar row. `updatedAt` may be an ISO string or an epoch
 * (seconds or ms — a bare number under 1e12 is treated as seconds). Assumption
 * flagged in operator.ts ConversationSummary — reconcile with the server shape. */
function relTime(v: string | number): string {
    const t = typeof v === 'number' ? (v < 1e12 ? v * 1000 : v) : Date.parse(v);
    if (!Number.isFinite(t)) return '';
    const s = Math.max(0, Math.floor((Date.now() - t) / 1000));
    if (s < 60) return 'just now';
    if (s < 3600) return `${Math.floor(s / 60)}m ago`;
    if (s < 86400) return `${Math.floor(s / 3600)}h ago`;
    return `${Math.floor(s / 86400)}d ago`;
}

/** The halo colour reads his mood: blue at rest, teal working, amber when he needs you. */
function haloColor(s: FaceState): string {
    if (s === 'awaiting') return 'var(--color-amber)';
    if (s === 'thinking' || s === 'speaking') return 'var(--color-th-teal)';
    return 'var(--color-th-blue)';
}

// ── Cost surfacing ───────────────────────────────────────────────────────────
// The cost of the active model is always on screen so spending is never a
// surprise. Badge + $/turn estimate degrade gracefully when the model-costs
// endpoint is unavailable (we fall back to the mode's tier). (th-2a6330)

function modeCost(mode: SmoothMode, costs: ModelCosts): ModelCost | undefined {
    return costs[mode.model];
}

/** Traffic-light badge for the active mode, or null when its cost is unknown. */
function modeBadge(mode: SmoothMode, costs: ModelCosts): string | null {
    const c = modeCost(mode, costs);
    return c ? costBadge(c.inputCostPerToken, c.outputCostPerToken) : null;
}

/** Expensive when the badge is 🧡/❤️; falls back to tier when cost is unknown. */
function modeExpensive(mode: SmoothMode, costs: ModelCosts): boolean {
    const badge = modeBadge(mode, costs);
    return badge ? isExpensiveBadge(badge) : mode.tier === 'premium';
}

/** A nominal 2k-token turn, in USD, from blended per-million rate. */
function estPerTurn(cost: ModelCost): number {
    return blendedPerMillion(cost) * 0.002;
}

function fmtUsd(n: number): string {
    return n >= 1 ? `$${n.toFixed(2)}` : `$${n.toFixed(4)}`;
}

/** The always-on model + cost readout: mode glyph/label, model id, badge, spend. */
function CostBar({ mode, costs, sessionCostUsd, className }: { mode: SmoothMode; costs: ModelCosts; sessionCostUsd: number; className?: string }) {
    const badge = modeBadge(mode, costs);
    const expensive = modeExpensive(mode, costs);
    return (
        <div className={`flex items-center gap-2 text-xs text-(--color-muted-foreground) ${className ?? ''}`}>
            <span className="font-medium text-foreground/80">
                {mode.emoji} {mode.label}
            </span>
            <span className="hidden font-mono opacity-70 sm:inline">{mode.model}</span>
            {badge && <span aria-hidden>{badge}</span>}
            <span className={`font-mono font-semibold ${expensive ? 'text-amber' : 'text-foreground/70'}`}>{fmtUsd(sessionCostUsd)}</span>
        </div>
    );
}

export default function App() {
    const {
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
        resumeConversation,
        newConversation,
        renameConversation,
    } = useOperator();
    const push = usePush();
    const [sidebarOpen, setSidebarOpen] = useState(false);
    const faceState: FaceState = state === 'connecting' || state === 'offline' ? 'idle' : (state as FaceState);
    const inConversation = messages.length > 0 || approvals.length > 0;

    // On mobile the drawer overlays, so a pick should dismiss it; on desktop it's
    // docked and stays. `lg` is 1024px — match that breakpoint.
    const closeOnMobile = () => {
        if (window.matchMedia('(max-width: 1023px)').matches) setSidebarOpen(false);
    };
    const onResume = (id: string) => {
        resumeConversation(id);
        closeOnMobile();
    };
    const onNew = () => {
        newConversation();
        closeOnMobile();
    };

    // Switching into the ❤️ `max` mode is a one-time, deliberate spend — confirm it.
    const guardedSetMode = useMemo(
        () => (id: string) => {
            if (id === 'max' && mode.id !== 'max' && !localStorage.getItem('smooth.mode.maxConfirmed')) {
                if (!window.confirm('Max mode (gpt-5.5-pro) costs roughly 200× Flash — continue?')) return;
                localStorage.setItem('smooth.mode.maxConfirmed', '1');
            }
            setMode(id);
        },
        [mode.id, setMode],
    );

    return (
        <>
            <Sidebar
                open={sidebarOpen}
                conversations={conversations}
                activeId={activeConversationId}
                onResume={onResume}
                onNew={onNew}
                onRename={renameConversation}
                onClose={() => setSidebarOpen(false)}
            />
            {/* Menu toggle — sits above the sidebar so it flips it open/closed from
                either state. The Smooth brand mark now lives in the sidebar header. */}
            <button
                onClick={() => setSidebarOpen((o) => !o)}
                aria-label={sidebarOpen ? 'Close conversations' : 'Open conversations'}
                title="Conversations"
                className="fixed top-3.5 left-4 z-50 grid size-9 place-items-center rounded-xl bg-panel/70 text-(--color-muted-foreground) backdrop-blur transition hover:text-foreground"
            >
                <Menu size={18} />
            </button>
            {/* Push to phone: enroll this device so Big Smooth can reach it with the
                tab closed. Hidden when the browser can't do Web Push, or once enabled. */}
            {push.supported && !push.enabled && (
                <button
                    onClick={() => void push.enable()}
                    disabled={push.busy}
                    title="Get notified on this device"
                    className="fixed top-4 right-4 z-10 flex items-center gap-1.5 rounded-full bg-panel/80 px-3 py-1.5 text-xs text-(--color-muted-foreground) opacity-70 transition hover:opacity-100 disabled:opacity-40"
                >
                    <Bell className="size-3.5" /> {push.busy ? 'Enabling…' : 'Notify me'}
                </button>
            )}
            {push.enabled && (
                <div
                    className="fixed top-4 right-4 z-10 flex items-center gap-1.5 text-xs text-(--color-online) opacity-60"
                    title="Notifications on for this device"
                >
                    <BellRing className="size-3.5" />
                </div>
            )}
            <div className={`h-dvh transition-[padding] duration-200 ${sidebarOpen ? 'lg:pl-72' : ''}`}>
                <div className="mx-auto flex h-dvh max-w-3xl flex-col overflow-hidden px-5">
                    {inConversation ? (
                        <>
                            <PresenceBar
                                state={state}
                                status={status}
                                faceState={faceState}
                                mode={mode}
                                modelCosts={modelCosts}
                                sessionCostUsd={sessionCostUsd}
                            />
                            <div className="presence-rule shrink-0" />
                            <main className="flex min-h-0 flex-1 flex-col">
                                <ApprovalDeck approvals={approvals} respond={respond} />
                                <Conversation messages={messages} approvals={approvals} />
                            </main>
                        </>
                    ) : (
                        <Greeting state={state} status={status} faceState={faceState} mode={mode} modelCosts={modelCosts} sessionCostUsd={sessionCostUsd} />
                    )}
                    <Composer
                        onSend={sendMessage}
                        disabled={state === 'connecting' || state === 'offline'}
                        mode={mode}
                        setMode={guardedSetMode}
                        modelCosts={modelCosts}
                    />
                </div>
            </div>
        </>
    );
}

/** The slide-in conversation sidebar: New chat + a list of recent conversations.
 * Overlays with a backdrop on mobile; docks (no backdrop) on desktop (lg). */
function Sidebar({
    open,
    conversations,
    activeId,
    onResume,
    onNew,
    onRename,
    onClose,
}: {
    open: boolean;
    conversations: ConversationSummary[];
    activeId: string | null;
    onResume: (id: string) => void;
    onNew: () => void;
    onRename: (id: string, title: string) => void;
    onClose: () => void;
}) {
    // Inline rename: which row is being edited + its draft text. Enter commits,
    // Esc (or blur) cancels. Double-clicking a title or the pencil opens it.
    const [editingId, setEditingId] = useState<string | null>(null);
    const [draft, setDraft] = useState('');
    const beginEdit = (id: string, current: string) => {
        setEditingId(id);
        setDraft(current);
    };
    const commitEdit = () => {
        if (editingId) onRename(editingId, draft);
        setEditingId(null);
    };
    return (
        <>
            {/* Backdrop — mobile only; a tap dismisses the drawer. */}
            {open && <div onClick={onClose} className="fixed inset-0 z-30 bg-background/60 backdrop-blur-sm lg:hidden" aria-hidden />}
            <aside
                className={`fixed top-0 left-0 z-40 flex h-dvh w-72 flex-col border-r border-border bg-panel/85 backdrop-blur-xl transition-transform duration-200 ${open ? 'translate-x-0' : '-translate-x-full'}`}
            >
                {/* pt-16 clears the floating menu button that overlaps this corner. */}
                <div className="flex items-center gap-2 px-4 pt-16 pb-3">
                    <a href="https://smoo.ai" target="_blank" rel="noreferrer" title="Smooth by Smoo AI" className="opacity-70 transition hover:opacity-100">
                        <img src="/smooth-icon.svg" alt="Smooth" className="size-5" />
                    </a>
                    <span className="text-sm font-semibold text-foreground/80">Conversations</span>
                </div>
                <button
                    onClick={onNew}
                    className="mx-3 mb-2 flex items-center justify-center gap-2 rounded-xl bg-coral px-4 py-2 text-sm font-semibold text-(--color-coral-ink) transition hover:brightness-110"
                >
                    <Plus size={16} /> New chat
                </button>
                <nav className="min-h-0 flex-1 space-y-0.5 overflow-y-auto px-2 pt-1 pb-4">
                    {conversations.length === 0 ? (
                        <p className="px-3 py-6 text-center text-xs text-(--color-muted-foreground)">No conversations yet.</p>
                    ) : (
                        conversations.map((c) => {
                            const active = c.conversationId === activeId;
                            const editing = editingId === c.conversationId;
                            return (
                                <div
                                    key={c.conversationId}
                                    className={`group relative rounded-xl transition ${
                                        active ? 'bg-(--color-th-teal)/12 ring-1 ring-(--color-th-teal)/40' : 'hover:bg-panel-2'
                                    }`}
                                >
                                    <button
                                        onClick={() => onResume(c.conversationId)}
                                        onDoubleClick={() => beginEdit(c.conversationId, c.title || '')}
                                        className="flex w-full flex-col gap-0.5 px-3 py-2 text-left"
                                    >
                                        <span className="flex items-center gap-2">
                                            <MessageSquare
                                                size={13}
                                                className={`shrink-0 ${active ? 'text-(--color-th-teal)' : 'text-(--color-muted-foreground)'}`}
                                            />
                                            <span className="truncate pr-6 text-sm font-medium">{c.title || 'Untitled'}</span>
                                        </span>
                                        <span className="pl-[21px] text-xs text-(--color-muted-foreground)">
                                            {relTime(c.updatedAt)}
                                            {c.messageCount ? ` · ${c.messageCount} msg` : ''}
                                        </span>
                                    </button>
                                    {/* Rename affordance — a hover pencil (or double-click the row).
                                        Overlaid as a sibling so it never nests inside the resume button. */}
                                    {!editing && (
                                        <button
                                            onClick={() => beginEdit(c.conversationId, c.title || '')}
                                            aria-label="Rename conversation"
                                            title="Rename"
                                            className="absolute top-2 right-2 grid size-6 place-items-center rounded-lg text-(--color-muted-foreground) opacity-0 transition group-hover:opacity-70 hover:bg-panel hover:!opacity-100"
                                        >
                                            <Pencil size={12} />
                                        </button>
                                    )}
                                    {editing && (
                                        <input
                                            autoFocus
                                            value={draft}
                                            onChange={(e) => setDraft(e.target.value)}
                                            onKeyDown={(e) => {
                                                if (e.key === 'Enter') commitEdit();
                                                else if (e.key === 'Escape') setEditingId(null);
                                            }}
                                            onBlur={commitEdit}
                                            aria-label="Conversation title"
                                            className="absolute inset-x-2 top-1.5 rounded-lg bg-panel-2 px-2 py-1 text-sm font-medium text-foreground outline-none ring-1 ring-(--color-th-teal)/50"
                                        />
                                    )}
                                </div>
                            );
                        })
                    )}
                </nav>
            </aside>
        </>
    );
}

/** "Big Smooth" in the official Smooth lockup — "Smoo" orange→red, "th"
 * (and "Big") teal→blue, so "Smooth" reads exactly like the brand mark. */
function Wordmark({ className }: { className?: string }) {
    return (
        <div className={`wordmark ${className ?? ''}`}>
            <span className="wm-th">Big</span> <span className="wm-smoo">Smoo</span>
            <span className="wm-th">th</span>
        </div>
    );
}

/** The face + its breathing halo, sized for the moment. */
// WebGL can be absent (headless browsers, locked-down devices, GPU blocklists).
// Three.js throws on context creation — without this, that error crashes the whole
// app. Degrade to a static brand-gradient orb so presence survives without 3D.
class FaceBoundary extends Component<{ size: number; children: ReactNode }, { failed: boolean }> {
    state = { failed: false };
    static getDerivedStateFromError() {
        return { failed: true };
    }
    render() {
        if (this.state.failed) return <div className="face-fallback" style={{ width: this.props.size, height: this.props.size }} />;
        return this.props.children;
    }
}

function FaceStage({ state, size, strong }: { state: FaceState; size: number; strong?: boolean }) {
    return (
        <div className="face-stage" style={{ width: size, height: size }}>
            <div className={`halo${strong ? ' halo-strong' : ''}`} style={{ '--halo': haloColor(state) } as React.CSSProperties} />
            <FaceBoundary size={size}>
                <BigSmoothFace state={state} size={size} />
            </FaceBoundary>
        </div>
    );
}

function StatusLine({ state, status, center }: { state: AgentState; status: Status; center?: boolean }) {
    const dot = !status.connected ? 'bg-(--color-muted-foreground)' : state === 'awaiting' ? 'bg-amber' : 'bg-(--color-online)';
    return (
        <div className={`flex items-center gap-2 text-sm text-(--color-muted-foreground) ${center ? 'justify-center' : ''}`}>
            <span className={`size-1.5 rounded-full ${dot}`} />
            <span className={state === 'awaiting' ? 'font-semibold text-amber' : ''}>{STATUS_CAPTION[state]}</span>
            {(state === 'thinking' || state === 'speaking') && (
                <span aria-hidden>
                    <span className="bs-dot">.</span>
                    <span className="bs-dot">.</span>
                    <span className="bs-dot">.</span>
                </span>
            )}
        </div>
    );
}

/** The empty-state greeting — he looks up when you walk in. */
function Greeting({
    state,
    status,
    faceState,
    mode,
    modelCosts,
    sessionCostUsd,
}: {
    state: AgentState;
    status: Status;
    faceState: FaceState;
    mode: SmoothMode;
    modelCosts: ModelCosts;
    sessionCostUsd: number;
}) {
    return (
        <main className="flex min-h-0 flex-1 flex-col items-center justify-center gap-7 pb-6 text-center">
            <FaceStage state={faceState} size={150} strong />
            <div className="flex flex-col items-center gap-3">
                <Wordmark className="text-4xl leading-none sm:text-[2.75rem]" />
                <StatusLine state={state} status={status} center />
                <CostBar mode={mode} costs={modelCosts} sessionCostUsd={sessionCostUsd} className="justify-center" />
            </div>
            <p className="max-w-sm text-balance text-[0.97rem] leading-relaxed text-(--color-muted-foreground)">
                {state === 'offline'
                    ? 'Reconnecting to your operator…'
                    : 'Your always-on operator. Ask him anything — or let his scheduled work bring things to you.'}
            </p>
        </main>
    );
}

/** The sticky presence once the conversation is underway. */
function PresenceBar({
    state,
    status,
    faceState,
    mode,
    modelCosts,
    sessionCostUsd,
}: {
    state: AgentState;
    status: Status;
    faceState: FaceState;
    mode: SmoothMode;
    modelCosts: ModelCosts;
    sessionCostUsd: number;
}) {
    const [, force] = useState(0);
    useEffect(() => {
        const id = setInterval(() => force((n) => n + 1), 30_000);
        return () => clearInterval(id);
    }, []);
    return (
        <header className="flex items-center gap-3.5 pt-5 pb-3">
            <FaceStage state={faceState} size={76} />
            <div className="min-w-0">
                <Wordmark className="text-[1.7rem] leading-none" />
                <div className="mt-1.5">
                    <StatusLine state={state} status={status} />
                </div>
            </div>
            <div className="ml-auto flex flex-col items-end gap-1 text-right text-xs text-(--color-muted-foreground)">
                <CostBar mode={mode} costs={modelCosts} sessionCostUsd={sessionCostUsd} className="justify-end" />
                <div>{status.connected ? `up ${uptime(status.since)}` : '—'}</div>
            </div>
        </header>
    );
}

function ApprovalDeck({ approvals, respond }: { approvals: Approval[]; respond: (id: string, ok: boolean) => void }) {
    if (!approvals.length) return null;
    return (
        <div className="mb-3 space-y-2 pt-3">
            {approvals.map((a) => (
                <div key={a.requestId} className="needs-you rounded-2xl bg-panel/90 p-4 backdrop-blur">
                    <div className="mb-2 flex items-center gap-2 text-sm font-semibold text-amber">
                        <span className="grid size-5 place-items-center rounded-full bg-amber/15">⚠</span>
                        Big Smooth needs a yes
                    </div>
                    <p className="mb-3 text-[0.95rem] leading-snug">
                        Run <span className="rounded-md bg-amber/10 px-1.5 py-0.5 font-mono text-[0.85em] text-amber">{a.tool}</span> — {a.description}
                    </p>
                    <div className="flex gap-2">
                        <button
                            onClick={() => respond(a.requestId, true)}
                            className="inline-flex items-center gap-1.5 rounded-full bg-coral px-4 py-1.5 text-sm font-semibold text-(--color-coral-ink) transition hover:brightness-110"
                        >
                            <Check size={15} /> Yes, go ahead
                        </button>
                        <button
                            onClick={() => respond(a.requestId, false)}
                            className="inline-flex items-center gap-1.5 rounded-full border border-border px-4 py-1.5 text-sm font-medium text-foreground/80 transition hover:bg-panel-2"
                        >
                            <X size={15} /> No
                        </button>
                    </div>
                </div>
            ))}
        </div>
    );
}

function Conversation({ messages, approvals }: { messages: ChatMessage[]; approvals: Approval[] }) {
    const ref = useRef<HTMLDivElement>(null);
    // Tools whose name has a pending approval are parked, not running.
    const awaiting = new Set(approvals.map((a) => a.tool));
    useEffect(() => {
        ref.current?.scrollTo({ top: ref.current.scrollHeight, behavior: 'smooth' });
    }, [messages]);

    return (
        <div ref={ref} className="min-h-0 flex-1 space-y-4 overflow-y-auto py-2">
            {messages.map((m) => (
                <MessageRow key={m.id} m={m} awaiting={awaiting} />
            ))}
        </div>
    );
}

function MessageRow({ m, awaiting }: { m: ChatMessage; awaiting: Set<string> }) {
    if (m.role === 'system') {
        return <div className="rounded-xl border border-amber/30 bg-amber/5 px-3 py-2 text-sm text-amber/90">{m.content}</div>;
    }
    if (m.role === 'user') {
        return (
            <div className="flex flex-col items-end gap-1.5">
                {m.attachments && m.attachments.length > 0 && (
                    <div className="flex max-w-[85%] flex-wrap justify-end gap-2">
                        {m.attachments.map((a, i) =>
                            a.mime.startsWith('image/') ? (
                                <img key={i} src={a.dataUrl} alt={a.name} className="max-h-64 rounded-lg" />
                            ) : (
                                <div key={i} className="flex items-center gap-2 rounded-lg border border-border bg-panel-2 px-3 py-2 text-sm">
                                    <FileText size={15} className="shrink-0 text-(--color-th-teal)" />
                                    <span className="max-w-40 truncate">{a.name}</span>
                                </div>
                            ),
                        )}
                    </div>
                )}
                {m.content && <div className="max-w-[85%] rounded-2xl rounded-br-md bg-coral/15 px-4 py-2.5 text-[0.95rem] leading-relaxed">{m.content}</div>}
            </div>
        );
    }
    // Render the turn as the model produced it: prose and tool chips interleaved
    // in arrival order (say a bit → call a tool → say a bit → final summary),
    // rather than every chip stacked above one concatenated wall of text.
    const lastIdx = m.blocks.length - 1;
    return (
        <div className="flex gap-3">
            <span className="mt-1 size-2 shrink-0 rounded-full bg-gradient-to-b from-(--color-th-teal) to-(--color-th-blue)" />
            <div className="min-w-0 flex-1 space-y-2">
                {m.reasoning && <Thinking text={m.reasoning} active={m.streaming && !m.content} />}
                {m.blocks.map((b, i) =>
                    b.kind === 'tool' ? (
                        <ToolChip key={b.tool.id} t={b.tool} awaiting={awaiting.has(b.tool.name)} />
                    ) : b.text.trim() ? (
                        <div
                            key={`t-${i}`}
                            className={`prose-msg text-[0.95rem] leading-relaxed text-foreground/95 ${m.streaming && i === lastIdx ? 'caret' : ''}`}
                        >
                            <Markdown remarkPlugins={[remarkGfm]}>{b.text}</Markdown>
                        </div>
                    ) : null,
                )}
                {m.streaming && m.blocks.length === 0 && <span className="caret text-(--color-muted-foreground)" />}
            </div>
        </div>
    );
}

// His thinking — captured on the reasoning channel, shown as a quiet,
// collapsible aside so it never competes with the answer. Open while he's
// still thinking, auto-collapses once the answer starts.
function Thinking({ text, active }: { text: string; active: boolean }) {
    return (
        <details open={active} className="mb-1.5">
            <summary className="inline-flex cursor-pointer list-none items-center gap-1.5 text-xs text-(--color-muted-foreground) transition select-none hover:text-foreground/70">
                <Brain size={12} className="shrink-0 text-(--color-th-teal)/70" />
                {active ? 'thinking…' : 'thought for a moment'}
            </summary>
            <div className="mt-1.5 border-l-2 border-(--color-th-teal)/25 pl-3 text-[0.82rem] leading-relaxed whitespace-pre-wrap text-(--color-muted-foreground) italic">
                {text}
            </div>
        </details>
    );
}

function ToolChip({ t, awaiting }: { t: ToolCall; awaiting: boolean }) {
    const Icon = TOOL_ICON[t.name] ?? Terminal;
    const arg = t.args.length > 80 ? `${t.args.slice(0, 80)}…` : t.args;
    return (
        <div className="my-1.5 overflow-hidden rounded-xl border border-border bg-panel/60">
            <div className="flex items-center gap-2 px-3 py-1.5 text-xs">
                <Icon size={13} className="shrink-0 text-(--color-th-teal)" />
                <span className="font-mono font-medium">{t.name}</span>
                <span className="truncate font-mono text-(--color-muted-foreground)">{arg}</span>
                <span className="ml-auto shrink-0">
                    {!t.done ? (
                        awaiting ? (
                            <span className="font-medium text-amber">awaiting your okay</span>
                        ) : (
                            <span className="text-(--color-muted-foreground)">running…</span>
                        )
                    ) : t.isError ? (
                        <X size={13} className="text-amber" />
                    ) : (
                        <Check size={13} className="text-coral" />
                    )}
                </span>
            </div>
            {t.done && t.result && (
                <pre className="max-h-32 overflow-y-auto border-t border-border/60 px-3 py-1.5 font-mono text-[0.72rem] leading-relaxed text-(--color-muted-foreground)">
                    {t.result.slice(0, 600)}
                </pre>
            )}
        </div>
    );
}

// ── Slash commands ───────────────────────────────────────────────────────────
// A small popup above the composer, mirroring the TUI's slash UX: type `/`,
// arrow + enter to pick, Esc to dismiss. The only command today is
// `/smooth-mode <preset>` (bare → lists presets with cost badges); it switches
// the active model and never sends a chat message. (th-f512b1)

const SLASH_COMMANDS = [{ name: 'smooth-mode', hint: 'switch the active model' }];

// ── @ mentions ───────────────────────────────────────────────────────────────
// A twin of the slash popup, but mid-text: type `@`, get workspace files / paths
// / pearls from Big Smooth's `/search` endpoint, arrow + enter to drop a
// reference in. Mirrors the `th code` TUI's `@` picker. Only one popup (slash or
// mention) is ever open — whichever token the caret sits in wins. (th-58b5fe)

const MENTION_GLYPH: Record<MentionResult['kind'], string> = {
    file: '📄',
    path: '📁',
    pearl: '◍',
};

type MenuItem = { kind: 'command'; name: string; hint: string } | { kind: 'preset'; mode: SmoothMode; badge: string | null };

interface SlashMenu {
    items: MenuItem[];
    /** Whether we're choosing the command itself or its preset argument. */
    stage: 'command' | 'preset';
}

function buildSlashMenu(text: string, costs: ModelCosts): SlashMenu | null {
    if (!text.startsWith('/')) return null;
    const rest = text.slice(1);
    const spaceIdx = rest.indexOf(' ');
    if (spaceIdx === -1) {
        const q = rest.toLowerCase();
        const items: MenuItem[] = SLASH_COMMANDS.filter((c) => c.name.startsWith(q)).map((c) => ({ kind: 'command', name: c.name, hint: c.hint }));
        return items.length ? { items, stage: 'command' } : null;
    }
    const cmd = rest.slice(0, spaceIdx);
    if (cmd === 'smooth-mode') {
        const arg = rest
            .slice(spaceIdx + 1)
            .trim()
            .toLowerCase();
        const items: MenuItem[] = MODES.filter((m) => m.id.toLowerCase().startsWith(arg) || m.label.toLowerCase().startsWith(arg)).map((m) => ({
            kind: 'preset',
            mode: m,
            badge: modeBadge(m, costs),
        }));
        return items.length ? { items, stage: 'preset' } : null;
    }
    return null;
}

function Composer({
    onSend,
    disabled,
    mode,
    setMode,
    modelCosts,
}: {
    onSend: (t: string, attachments: Attachment[]) => void;
    disabled: boolean;
    mode: SmoothMode;
    setMode: (id: string) => void;
    modelCosts: ModelCosts;
}) {
    const [text, setText] = useState('');
    const [caret, setCaret] = useState(0);
    const [sel, setSel] = useState(0);
    const [dismissed, setDismissed] = useState(false);
    const [attachments, setAttachments] = useState<Attachment[]>([]);
    const [dragging, setDragging] = useState(false);
    const taRef = useRef<HTMLTextAreaElement>(null);
    const fileRef = useRef<HTMLInputElement>(null);

    // Read picked/pasted/dropped images + PDFs as data-URLs (prefix kept — it's
    // the wire value AND the preview src). Images are DOWNSCALED to <=1568px on
    // the longest edge and re-encoded JPEG first: a raw phone photo is multiple
    // MB, which times out the LLM gateway (524); vision models gain nothing past
    // ~1568px. PDFs pass through unchanged. Pearl th-3be564.
    const addFiles = (files: FileList | File[] | null) => {
        if (!files) return;
        const push = (name: string, mime: string, dataUrl: string) => setAttachments((prev) => [...prev, { name, mime, dataUrl }]);
        const readRaw = (file: File) => {
            const reader = new FileReader();
            reader.onload = () => push(file.name, file.type, reader.result as string);
            reader.readAsDataURL(file);
        };
        for (const file of Array.from(files)) {
            const isImage = /^image\//.test(file.type);
            if (!isImage && file.type !== 'application/pdf') continue;
            if (isImage) {
                downscaleImage(file)
                    .then((dataUrl) => push(file.name, 'image/jpeg', dataUrl))
                    .catch(() => readRaw(file)); // fall back to the original on any canvas failure
            } else {
                readRaw(file);
            }
        }
    };
    const removeAttachment = (i: number) => setAttachments((prev) => prev.filter((_, j) => j !== i));

    // The `@` token under the caret (if any) drives the mention popup. When it's
    // active we suppress the `/` slash menu — only the token the caret sits in wins.
    const mention = useMemo(() => activeMention(text, caret), [text, caret]);
    const mentionQuery = mention && !dismissed ? mention.query : null;
    const mentionResults = useMentionSearch(mentionQuery);
    const mentionVisible = !!mention && !dismissed && mentionResults.length > 0;
    const mentionSel = Math.min(sel, Math.max(0, mentionResults.length - 1));

    const menu = useMemo(() => (dismissed || mention ? null : buildSlashMenu(text, modelCosts)), [text, modelCosts, dismissed, mention]);
    const selItem = menu ? menu.items[Math.min(sel, menu.items.length - 1)] : null;

    const cost = modeCost(mode, modelCosts);
    const expensive = modeExpensive(mode, modelCosts);

    // Reset the highlighted row whenever the mention query changes so a stale
    // index from a previous result set never points past the new list.
    useEffect(() => {
        setSel(0);
    }, [mentionQuery]);

    // Whole-window drag-and-drop: drop an image/PDF ANYWHERE on the PWA to
    // attach it, not just onto the composer strip. A depth counter avoids
    // flicker as the pointer crosses child elements; we only react to file
    // drags (`types` includes 'Files') so internal text/element drags are
    // ignored. `addFiles` uses functional setState, so the stale closure is
    // safe. Pearl th-3be564 (drop-anywhere).
    useEffect(() => {
        let depth = 0;
        const hasFiles = (e: DragEvent) => Array.from(e.dataTransfer?.types ?? []).includes('Files');
        const onEnter = (e: DragEvent) => {
            if (!hasFiles(e)) return;
            e.preventDefault();
            depth += 1;
            if (!disabled) setDragging(true);
        };
        // preventDefault on dragover is REQUIRED or the browser navigates to the
        // file instead of firing `drop`.
        const onOver = (e: DragEvent) => {
            if (hasFiles(e)) e.preventDefault();
        };
        const onLeave = (e: DragEvent) => {
            if (!hasFiles(e)) return;
            depth = Math.max(0, depth - 1);
            if (depth === 0) setDragging(false);
        };
        const onDrop = (e: DragEvent) => {
            if (!hasFiles(e)) return;
            e.preventDefault();
            depth = 0;
            setDragging(false);
            if (!disabled) addFiles(e.dataTransfer?.files ?? null);
        };
        window.addEventListener('dragenter', onEnter);
        window.addEventListener('dragover', onOver);
        window.addEventListener('dragleave', onLeave);
        window.addEventListener('drop', onDrop);
        return () => {
            window.removeEventListener('dragenter', onEnter);
            window.removeEventListener('dragover', onOver);
            window.removeEventListener('dragleave', onLeave);
            window.removeEventListener('drop', onDrop);
        };
    }, [disabled]);

    const update = (next: string) => {
        setText(next);
        setSel(0);
        setDismissed(false);
    };

    // Replace the whole `@<query>` token with the picked result's value, leaving
    // a trailing space, and restore the caret just past it.
    const insertMention = (result: MentionResult) => {
        if (!mention) return;
        const before = text.slice(0, mention.start);
        const after = text.slice(mention.tokenEnd);
        const insert = result.value + (after.startsWith(' ') ? '' : ' ');
        const next = before + insert + after;
        const pos = before.length + insert.length;
        update(next);
        setCaret(pos);
        requestAnimationFrame(() => {
            const ta = taRef.current;
            if (ta) {
                ta.focus();
                ta.setSelectionRange(pos, pos);
            }
        });
    };

    const applyItem = (item: MenuItem) => {
        if (item.kind === 'command') {
            // Autocomplete to the argument stage; keep the menu open.
            update(`/${item.name} `);
            taRef.current?.focus();
        } else {
            // Switch modes — this is NOT a chat message.
            setMode(item.mode.id);
            update('');
        }
    };

    const submit = () => {
        if (menu && selItem) {
            applyItem(selItem);
            return;
        }
        if (!text.trim() && attachments.length === 0) return;
        onSend(text, attachments);
        setAttachments([]);
        update('');
    };

    return (
        <div className="relative pb-5 pt-1">
            {/* Drag-anywhere: full-screen cue while a file is over the PWA. The
                window-level listeners (above) do the actual attach; this overlay
                is purely visual (pointer-events-none so it never eats the drop). */}
            {dragging && (
                <div className="pointer-events-none fixed inset-0 z-50 grid place-items-center bg-background/70 backdrop-blur-sm">
                    <div className="rounded-2xl border-2 border-dashed border-(--color-th-teal) bg-panel/80 px-8 py-6 text-center shadow-xl">
                        <Paperclip size={28} className="mx-auto mb-2 text-(--color-th-teal)" />
                        <p className="text-lg font-semibold">Drop to attach</p>
                        <p className="text-sm text-(--color-muted-foreground)">images &amp; PDFs</p>
                    </div>
                </div>
            )}
            {mentionVisible && (
                <div className="absolute right-0 bottom-full left-0 mb-2 overflow-hidden rounded-2xl border border-border bg-panel/95 shadow-xl backdrop-blur">
                    <ul className="max-h-72 overflow-y-auto py-1">
                        {mentionResults.map((r, i) => {
                            const active = i === mentionSel;
                            return (
                                <li key={`${r.kind}-${r.value}-${i}`}>
                                    <button
                                        type="button"
                                        onMouseDown={(e) => {
                                            e.preventDefault();
                                            insertMention(r);
                                        }}
                                        onMouseEnter={() => setSel(i)}
                                        className={`flex w-full items-center gap-2.5 px-3 py-1.5 text-left text-sm transition ${active ? 'bg-panel-2' : ''}`}
                                    >
                                        <span aria-hidden className="w-5 shrink-0 text-center">
                                            {MENTION_GLYPH[r.kind]}
                                        </span>
                                        <span className="truncate font-medium">{r.label}</span>
                                        {r.detail && <span className="truncate font-mono text-xs text-(--color-muted-foreground)">{r.detail}</span>}
                                    </button>
                                </li>
                            );
                        })}
                    </ul>
                </div>
            )}
            {menu && (
                <div className="absolute right-0 bottom-full left-0 mb-2 overflow-hidden rounded-2xl border border-border bg-panel/95 shadow-xl backdrop-blur">
                    {menu.stage === 'preset' && (
                        <div className="border-b border-border/60 px-3 py-1.5 text-[0.7rem] font-semibold tracking-wide text-(--color-muted-foreground) uppercase">
                            Smooth Modes
                        </div>
                    )}
                    <ul className="max-h-72 overflow-y-auto py-1">
                        {menu.items.map((item, i) => {
                            const active = i === Math.min(sel, menu.items.length - 1);
                            const key = item.kind === 'command' ? `c-${item.name}` : `m-${item.mode.id}`;
                            return (
                                <li key={key}>
                                    <button
                                        type="button"
                                        onMouseDown={(e) => {
                                            e.preventDefault();
                                            applyItem(item);
                                        }}
                                        onMouseEnter={() => setSel(i)}
                                        className={`flex w-full items-center gap-2.5 px-3 py-1.5 text-left text-sm transition ${active ? 'bg-panel-2' : ''}`}
                                    >
                                        {item.kind === 'command' ? (
                                            <>
                                                <span className="font-mono font-medium text-(--color-th-teal)">/{item.name}</span>
                                                <span className="text-xs text-(--color-muted-foreground)">{item.hint}</span>
                                            </>
                                        ) : (
                                            <>
                                                <span aria-hidden className="w-5 text-center">
                                                    {item.mode.emoji}
                                                </span>
                                                <span className="font-medium">{item.mode.label}</span>
                                                <span className="font-mono text-xs text-(--color-muted-foreground)">{item.mode.model}</span>
                                                {item.badge && (
                                                    <span aria-hidden className="ml-auto">
                                                        {item.badge}
                                                    </span>
                                                )}
                                                {item.mode.id === mode.id && (
                                                    <span className={`text-xs text-(--color-th-teal) ${item.badge ? '' : 'ml-auto'}`}>active</span>
                                                )}
                                            </>
                                        )}
                                    </button>
                                </li>
                            );
                        })}
                    </ul>
                </div>
            )}
            <input
                ref={fileRef}
                type="file"
                accept="image/*,application/pdf"
                multiple
                className="hidden"
                onChange={(e) => {
                    addFiles(e.target.files);
                    e.target.value = '';
                }}
            />
            <div
                className={`rounded-2xl border bg-panel/70 p-2 backdrop-blur focus-within:border-(--color-th-teal)/50 ${dragging ? 'border-(--color-th-teal) bg-(--color-th-teal)/5' : 'border-border'}`}
            >
                {attachments.length > 0 && (
                    <div className="mb-2 flex flex-wrap gap-2 px-1 pt-1">
                        {attachments.map((a, i) => (
                            <div key={`${a.name}-${i}`} className="relative">
                                {a.mime.startsWith('image/') ? (
                                    <img src={a.dataUrl} alt={a.name} className="size-16 rounded-lg object-cover" />
                                ) : (
                                    <div className="flex size-16 flex-col items-center justify-center gap-1 rounded-lg border border-border bg-panel-2 p-1 text-center">
                                        <FileText size={18} className="shrink-0 text-(--color-th-teal)" />
                                        <span className="w-full truncate text-[0.6rem] text-(--color-muted-foreground)">{a.name}</span>
                                    </div>
                                )}
                                <button
                                    type="button"
                                    onClick={() => removeAttachment(i)}
                                    aria-label={`Remove ${a.name}`}
                                    className="absolute -right-1.5 -top-1.5 grid size-5 place-items-center rounded-full border border-border bg-panel-2 text-foreground/80 transition hover:brightness-125"
                                >
                                    <X size={11} />
                                </button>
                            </div>
                        ))}
                    </div>
                )}
                <div className="flex items-end gap-2">
                    <button
                        type="button"
                        onClick={() => fileRef.current?.click()}
                        disabled={disabled}
                        aria-label="Attach image or PDF"
                        title="Attach image or PDF"
                        className="grid size-9 shrink-0 place-items-center rounded-xl text-(--color-muted-foreground) transition enabled:hover:text-foreground disabled:opacity-40"
                    >
                        <Paperclip size={18} />
                    </button>
                    <textarea
                        ref={taRef}
                        value={text}
                        onChange={(e) => {
                            update(e.target.value);
                            setCaret(e.target.selectionStart ?? e.target.value.length);
                        }}
                        onPaste={(e) => {
                            const usable = Array.from(e.clipboardData.files).filter((f) => /^image\//.test(f.type) || f.type === 'application/pdf');
                            if (usable.length) {
                                e.preventDefault();
                                addFiles(usable);
                            }
                        }}
                        onSelect={(e) => setCaret(e.currentTarget.selectionStart ?? 0)}
                        onKeyDown={(e) => {
                            if (mentionVisible) {
                                if (e.key === 'ArrowDown') {
                                    e.preventDefault();
                                    setSel((s) => (s + 1) % mentionResults.length);
                                    return;
                                }
                                if (e.key === 'ArrowUp') {
                                    e.preventDefault();
                                    setSel((s) => (s - 1 + mentionResults.length) % mentionResults.length);
                                    return;
                                }
                                if (e.key === 'Enter' || e.key === 'Tab') {
                                    e.preventDefault();
                                    insertMention(mentionResults[mentionSel]);
                                    return;
                                }
                                if (e.key === 'Escape') {
                                    e.preventDefault();
                                    setDismissed(true);
                                    return;
                                }
                            } else if (menu && menu.items.length) {
                                if (e.key === 'ArrowDown') {
                                    e.preventDefault();
                                    setSel((s) => (s + 1) % menu.items.length);
                                    return;
                                }
                                if (e.key === 'ArrowUp') {
                                    e.preventDefault();
                                    setSel((s) => (s - 1 + menu.items.length) % menu.items.length);
                                    return;
                                }
                                if (e.key === 'Tab') {
                                    e.preventDefault();
                                    if (selItem) applyItem(selItem);
                                    return;
                                }
                                if (e.key === 'Escape') {
                                    e.preventDefault();
                                    setDismissed(true);
                                    return;
                                }
                            }
                            if (e.key === 'Enter' && !e.shiftKey) {
                                e.preventDefault();
                                submit();
                            }
                        }}
                        rows={1}
                        placeholder={disabled ? 'Waiting for your operator…' : 'Talk to Big Smooth…  (/ for modes · @ to mention)'}
                        disabled={disabled}
                        className="max-h-40 flex-1 resize-none bg-transparent px-2 py-1.5 text-[0.95rem] outline-none placeholder:text-(--color-muted-foreground)"
                    />
                    <button
                        onClick={submit}
                        disabled={disabled || (!text.trim() && attachments.length === 0)}
                        className="grid size-9 shrink-0 place-items-center rounded-xl bg-coral text-(--color-coral-ink) transition enabled:hover:brightness-110 disabled:opacity-40"
                        aria-label="Send"
                    >
                        <ArrowUp size={18} />
                    </button>
                </div>
            </div>
            {expensive && (
                <div className="mt-2 flex items-center gap-1.5 rounded-xl border border-amber/30 bg-amber/5 px-3 py-1.5 text-xs font-medium text-amber">
                    <span aria-hidden>⚠</span>
                    <span>
                        PREMIUM — {mode.emoji} {mode.label} · {cost ? `~${fmtUsd(estPerTurn(cost))}/turn` : 'premium rates'}
                    </span>
                </div>
            )}
        </div>
    );
}

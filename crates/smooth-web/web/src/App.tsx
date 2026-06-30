// The Presence control surface — Big Smooth as a character you cohabit with.
// His face is the room: he greets you large and centred, then settles into a
// persistent presence bar once you're talking. A halo breathes behind him and
// shifts colour with his state, so the "needs you" moment emanates from him
// rather than a detached banner. A thin client on the operator's canonical
// protocol (EPIC th-c89c2a, th-f1a1f0, th-833b5f).

import { ArrowUp, Check, X, Terminal, FileText, Search, Folder, Pencil, Brain } from 'lucide-react';
import { useEffect, useMemo, useRef, useState } from 'react';
import Markdown from 'react-markdown';
import remarkGfm from 'remark-gfm';

import { BigSmoothFace, type FaceState } from './components/BigSmoothFace';
import { MODES, costBadge, isExpensiveBadge, blendedPerMillion, type SmoothMode, type ModelCost, type ModelCosts } from './modes';
import { useOperator, type AgentState, type ChatMessage, type ToolCall, type Approval, type Status } from './operator';
import { useMentionSearch, activeMention, type MentionResult } from './useMentionSearch';

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

function uptime(since: number): string {
    const s = Math.max(0, Math.floor((Date.now() - since) / 1000));
    if (s < 60) return `${s}s`;
    if (s < 3600) return `${Math.floor(s / 60)}m`;
    if (s < 86400) return `${Math.floor(s / 3600)}h`;
    return `${Math.floor(s / 86400)}d`;
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
    const { state, messages, approvals, status, sendMessage, respond, mode, setMode, sessionCostUsd, modelCosts } = useOperator();
    const faceState: FaceState = state === 'connecting' || state === 'offline' ? 'idle' : (state as FaceState);
    const inConversation = messages.length > 0 || approvals.length > 0;

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
            {/* The Smooth product mark — a quiet corner lockup that never competes
                with Big Smooth himself. Hidden on small screens where space is tight. */}
            <a
                href="https://smoo.ai"
                target="_blank"
                rel="noreferrer"
                title="Smooth by Smoo AI"
                className="fixed top-4 left-4 z-10 hidden opacity-35 transition hover:opacity-90 sm:block"
            >
                <img src="/smooth-icon.svg" alt="Smooth" className="size-6" />
            </a>
            <div className="mx-auto flex h-full max-w-3xl flex-col px-5">
                {inConversation ? (
                    <>
                        <PresenceBar state={state} status={status} faceState={faceState} mode={mode} modelCosts={modelCosts} sessionCostUsd={sessionCostUsd} />
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
function FaceStage({ state, size, strong }: { state: FaceState; size: number; strong?: boolean }) {
    return (
        <div className="face-stage" style={{ width: size, height: size }}>
            <div className={`halo${strong ? ' halo-strong' : ''}`} style={{ '--halo': haloColor(state) } as React.CSSProperties} />
            <BigSmoothFace state={state} size={size} />
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
            <div className="flex justify-end">
                <div className="max-w-[85%] rounded-2xl rounded-br-md bg-coral/15 px-4 py-2.5 text-[0.95rem] leading-relaxed">{m.content}</div>
            </div>
        );
    }
    return (
        <div className="flex gap-3">
            <span className="mt-1 size-2 shrink-0 rounded-full bg-gradient-to-b from-(--color-th-teal) to-(--color-th-blue)" />
            <div className="min-w-0 flex-1">
                {m.reasoning && <Thinking text={m.reasoning} active={m.streaming && !m.content} />}
                {m.tools.map((t) => (
                    <ToolChip key={t.id} t={t} awaiting={awaiting.has(t.name)} />
                ))}
                {m.content && (
                    <div className={`prose-msg text-[0.95rem] leading-relaxed text-foreground/95 ${m.streaming ? 'caret' : ''}`}>
                        <Markdown remarkPlugins={[remarkGfm]}>{m.content}</Markdown>
                    </div>
                )}
                {m.streaming && !m.content && !m.tools.length && <span className="caret text-(--color-muted-foreground)" />}
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
    onSend: (t: string) => void;
    disabled: boolean;
    mode: SmoothMode;
    setMode: (id: string) => void;
    modelCosts: ModelCosts;
}) {
    const [text, setText] = useState('');
    const [caret, setCaret] = useState(0);
    const [sel, setSel] = useState(0);
    const [dismissed, setDismissed] = useState(false);
    const taRef = useRef<HTMLTextAreaElement>(null);

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
        if (!text.trim()) return;
        onSend(text);
        update('');
    };

    return (
        <div className="relative pb-5 pt-1">
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
            <div className="flex items-end gap-2 rounded-2xl border border-border bg-panel/70 p-2 backdrop-blur focus-within:border-(--color-th-teal)/50">
                <textarea
                    ref={taRef}
                    value={text}
                    onChange={(e) => {
                        update(e.target.value);
                        setCaret(e.target.selectionStart ?? e.target.value.length);
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
                    disabled={disabled || !text.trim()}
                    className="grid size-9 shrink-0 place-items-center rounded-xl bg-coral text-(--color-coral-ink) transition enabled:hover:brightness-110 disabled:opacity-40"
                    aria-label="Send"
                >
                    <ArrowUp size={18} />
                </button>
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

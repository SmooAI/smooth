// The Presence control surface — Big Smooth as a character you cohabit with.
// His face is the room: he greets you large and centred, then settles into a
// persistent presence bar once you're talking. A halo breathes behind him and
// shifts colour with his state, so the "needs you" moment emanates from him
// rather than a detached banner. A thin client on the operator's canonical
// protocol (EPIC th-c89c2a, th-f1a1f0, th-833b5f).

import { useEffect, useRef, useState } from 'react';
import Markdown from 'react-markdown';
import remarkGfm from 'remark-gfm';
import { ArrowUp, Check, X, Terminal, FileText, Search, Folder, Pencil, Brain } from 'lucide-react';

import { BigSmoothFace, type FaceState } from './components/BigSmoothFace';
import { useOperator, type AgentState, type ChatMessage, type ToolCall, type Approval, type Status } from './operator';

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

export default function App() {
    const { state, messages, approvals, status, sendMessage, respond } = useOperator();
    const faceState: FaceState = state === 'connecting' || state === 'offline' ? 'idle' : (state as FaceState);
    const inConversation = messages.length > 0 || approvals.length > 0;

    return (
        <div className="mx-auto flex h-full max-w-3xl flex-col px-5">
            {inConversation ? (
                <>
                    <PresenceBar state={state} status={status} faceState={faceState} />
                    <div className="presence-rule shrink-0" />
                    <main className="flex min-h-0 flex-1 flex-col">
                        <ApprovalDeck approvals={approvals} respond={respond} />
                        <Conversation messages={messages} approvals={approvals} />
                    </main>
                </>
            ) : (
                <Greeting state={state} status={status} faceState={faceState} />
            )}
            <Composer onSend={sendMessage} disabled={state === 'connecting' || state === 'offline'} />
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
function Greeting({ state, status, faceState }: { state: AgentState; status: Status; faceState: FaceState }) {
    return (
        <main className="flex min-h-0 flex-1 flex-col items-center justify-center gap-7 pb-6 text-center">
            <FaceStage state={faceState} size={150} strong />
            <div className="flex flex-col items-center gap-3">
                <div className="wordmark text-4xl leading-none sm:text-[2.75rem]">Big Smooth</div>
                <StatusLine state={state} status={status} center />
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
function PresenceBar({ state, status, faceState }: { state: AgentState; status: Status; faceState: FaceState }) {
    const [, force] = useState(0);
    useEffect(() => {
        const id = setInterval(() => force((n) => n + 1), 30_000);
        return () => clearInterval(id);
    }, []);
    return (
        <header className="flex items-center gap-3.5 pt-5 pb-3">
            <FaceStage state={faceState} size={76} />
            <div className="min-w-0">
                <div className="wordmark text-[1.7rem] leading-none">Big Smooth</div>
                <div className="mt-1.5">
                    <StatusLine state={state} status={status} />
                </div>
            </div>
            <div className="ml-auto hidden text-right text-xs text-(--color-muted-foreground) sm:block">
                {status.model && <div className="font-medium text-foreground/80">{status.model}</div>}
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
                <pre className="max-h-32 overflow-y-auto border-t border-border/60 px-3 py-1.5 font-mono text-[0.72rem] leading-relaxed text-(--color-muted-foreground)">{t.result.slice(0, 600)}</pre>
            )}
        </div>
    );
}

function Composer({ onSend, disabled }: { onSend: (t: string) => void; disabled: boolean }) {
    const [text, setText] = useState('');
    const submit = () => {
        if (!text.trim()) return;
        onSend(text);
        setText('');
    };
    return (
        <div className="pb-5 pt-1">
            <div className="flex items-end gap-2 rounded-2xl border border-border bg-panel/70 p-2 backdrop-blur focus-within:border-(--color-th-teal)/50">
                <textarea
                    value={text}
                    onChange={(e) => setText(e.target.value)}
                    onKeyDown={(e) => {
                        if (e.key === 'Enter' && !e.shiftKey) {
                            e.preventDefault();
                            submit();
                        }
                    }}
                    rows={1}
                    placeholder={disabled ? 'Waiting for your operator…' : 'Talk to Big Smooth…'}
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
        </div>
    );
}

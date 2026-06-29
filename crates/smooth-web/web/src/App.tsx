// The Presence control surface — Big Smooth as a character you cohabit with.
// A reactive face anchors the room; the amber "needs you" deck is the hero; the
// live conversation flows below. A thin client on the operator's canonical
// protocol (EPIC th-c89c2a, th-f1a1f0).

import { useEffect, useRef, useState } from 'react';
import Markdown from 'react-markdown';
import remarkGfm from 'remark-gfm';
import { ArrowUp, Check, X, Terminal, FileText, Search, Folder, Pencil } from 'lucide-react';

import { BigSmoothFace, type FaceState } from './components/BigSmoothFace';
import { useOperator, type AgentState, type ChatMessage, type ToolCall, type Approval } from './operator';

const STATUS_CAPTION: Record<AgentState, string> = {
    connecting: 'waking up',
    offline: 'offline — reconnecting',
    awake: 'awake',
    thinking: 'thinking',
    speaking: 'speaking',
    awaiting: 'needs your okay',
};

function uptime(since: number): string {
    const s = Math.max(0, Math.floor((Date.now() - since) / 1000));
    if (s < 60) return `${s}s`;
    if (s < 3600) return `${Math.floor(s / 60)}m`;
    if (s < 86400) return `${Math.floor(s / 3600)}h`;
    return `${Math.floor(s / 86400)}d`;
}

const TOOL_ICON: Record<string, typeof Terminal> = {
    bash: Terminal,
    read_file: FileText,
    write_file: Pencil,
    edit_file: Pencil,
    grep: Search,
    list_files: Folder,
};

export default function App() {
    const { state, messages, approvals, status, sendMessage, respond } = useOperator();
    const faceState: FaceState = state === 'connecting' || state === 'offline' ? 'idle' : (state as FaceState);

    return (
        <div className="mx-auto flex h-full max-w-3xl flex-col px-4">
            <Header state={state} status={status} faceState={faceState} />
            <main className="flex min-h-0 flex-1 flex-col">
                <ApprovalDeck approvals={approvals} respond={respond} />
                <Conversation messages={messages} state={state} approvals={approvals} />
            </main>
            <Composer onSend={sendMessage} disabled={state === 'connecting' || state === 'offline'} />
        </div>
    );
}

function Header({ state, status, faceState }: { state: AgentState; status: ReturnType<typeof useOperator>['status']; faceState: FaceState }) {
    const [, force] = useState(0);
    useEffect(() => {
        const id = setInterval(() => force((n) => n + 1), 30_000);
        return () => clearInterval(id);
    }, []);
    // Green = alive & awake; amber = needs you; dim = offline.
    const dot = !status.connected ? 'bg-(--color-muted-foreground)' : state === 'awaiting' ? 'bg-amber' : 'bg-(--color-online)';
    return (
        <header className="flex items-center gap-4 pt-6 pb-4">
            <BigSmoothFace state={faceState} size={72} />
            <div className="min-w-0">
                <div className="wordmark text-2xl leading-none">Big Smooth</div>
                <div className="mt-1.5 flex items-center gap-2 text-sm text-(--color-muted-foreground)">
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
        <div className="mb-3 space-y-2">
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

function Conversation({ messages, state, approvals }: { messages: ChatMessage[]; state: AgentState; approvals: Approval[] }) {
    const ref = useRef<HTMLDivElement>(null);
    // Tools whose name has a pending approval are parked, not running.
    const awaiting = new Set(approvals.map((a) => a.tool));
    useEffect(() => {
        ref.current?.scrollTo({ top: ref.current.scrollHeight, behavior: 'smooth' });
    }, [messages]);

    if (!messages.length) {
        return (
            <div ref={ref} className="flex min-h-0 flex-1 flex-col items-center justify-center gap-2 text-center text-(--color-muted-foreground)">
                <p className="text-lg text-foreground/70">{state === 'offline' ? 'Reconnecting to your operator…' : 'Big Smooth is awake.'}</p>
                <p className="text-sm">Ask him anything, or let his scheduled tasks bring things to you.</p>
            </div>
        );
    }
    return (
        <div ref={ref} className="min-h-0 flex-1 space-y-4 overflow-y-auto pb-4">
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

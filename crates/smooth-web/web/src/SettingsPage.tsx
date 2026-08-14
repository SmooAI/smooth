//! The Settings tab — the controls that were previously only reachable through
//! the composer slash-menu / localStorage, plus connection + access info.
//!
//! Presence styling: teal marks the *active* choice (a live selection is the one
//! "present" thing on the page); amber is never used — settings never demand you.

import { Bell, KeyRound, Link2, Scale, ShieldCheck } from 'lucide-react';
import { useCallback, useEffect, useState } from 'react';

import { ModelPicker } from './ModelSelector';
import { MODES, type ModelCosts, type SmoothMode } from './modes';
import { resolveTarget, type Status } from './operator';
import type { PushApi } from './usePush';

/** A Big Smooth daemon discovered on the tailnet (mirrors the Electron shape). */
interface RemoteDaemon {
    name: string;
    url: string;
    identity?: string;
}

/** The narrow bridge the Electron preload injects; absent in the browser PWA. */
interface BigSmoothBridge {
    listDaemons: () => Promise<{ current: string | null; daemons: RemoteDaemon[] }>;
    connectTo: (url: string | null) => Promise<void>;
    /** Cmd/Ctrl+N from the desktop window → new chat. Returns an unsubscribe fn.
     * Absent in the browser PWA (Electron-only). */
    onNewChat?: (cb: () => void) => () => void;
    /** Show a native OS notification (th-6af4b1). Electron-only — the webview
     * can't do Web Push, so `usePush` falls back to this. Resolves to shown. */
    notify?: (payload: { title?: string; body?: string; deepLink?: string }) => Promise<boolean>;
}
declare global {
    interface Window {
        bigSmooth?: BigSmoothBridge;
    }
}

/** One connect target row (This Mac / a tailnet daemon). Teal = the active one. */
function TargetRow({ label, active, disabled, onPick }: { label: string; active: boolean; disabled: boolean; onPick: () => void }) {
    return (
        <button
            type="button"
            disabled={disabled || active}
            onClick={onPick}
            className={`flex w-full items-center gap-2.5 rounded-xl border px-3 py-2 text-left text-sm transition ${
                active
                    ? 'border-transparent bg-(--color-th-teal)/12 text-foreground ring-1 ring-(--color-th-teal)/40'
                    : 'border-border bg-panel/60 text-foreground/80 hover:bg-panel-2 disabled:opacity-60'
            }`}
        >
            <span className={`size-2 rounded-full ${active ? 'bg-(--color-th-teal)' : 'bg-(--color-muted-foreground)/40'}`} />
            {label}
        </button>
    );
}

/** Switch which daemon the window is attached to — This Mac or a discovered
 * tailnet daemon (e.g. smoo-hub). Electron-only (drives the app via the preload
 * bridge); in the browser PWA it just points at the menu-bar app. */
function ConnectionSwitcher() {
    const bridge = typeof window === 'undefined' ? undefined : window.bigSmooth;
    const [state, setState] = useState<{ current: string | null; daemons: RemoteDaemon[] } | null>(null);
    const [busy, setBusy] = useState(false);

    const load = useCallback(() => {
        if (bridge)
            void bridge
                .listDaemons()
                .then(setState)
                .catch(() => setState({ current: null, daemons: [] }));
    }, [bridge]);
    useEffect(load, [load]);

    if (!bridge) {
        return (
            <p className="mb-3 text-xs text-(--color-muted-foreground)">
                Switch between this Mac and tailnet daemons from the Big Smooth menu-bar icon → <span className="text-foreground/80">Connect</span>.
            </p>
        );
    }

    const active = state?.current ?? null;
    const pick = (url: string | null) => {
        setBusy(true);
        void bridge.connectTo(url); // the app relaunches onto the new target
    };

    return (
        <div className="mb-3">
            <div className="mb-2 flex items-center justify-between">
                <span className="text-xs uppercase tracking-wide text-(--color-muted-foreground)">Connect to</span>
                <button type="button" onClick={load} className="text-xs text-(--color-muted-foreground) transition hover:text-foreground">
                    Refresh
                </button>
            </div>
            <div className="space-y-1">
                <TargetRow label="This Mac (local)" active={active === null} disabled={busy} onPick={() => pick(null)} />
                {state?.daemons.map((d) => (
                    <TargetRow
                        key={d.url}
                        label={d.identity ? `${d.name} — ${d.identity}` : d.name}
                        active={active === d.url}
                        disabled={busy}
                        onPick={() => pick(d.url)}
                    />
                ))}
            </div>
            {busy && <p className="mt-2 text-xs text-(--color-muted-foreground)">Reconnecting — the app will relaunch.</p>}
        </div>
    );
}

function Section({ icon, title, children }: { icon: React.ReactNode; title: string; children: React.ReactNode }) {
    return (
        <section className="mt-6">
            <h2 className="mb-3 flex items-center gap-2 text-sm font-semibold text-foreground/90">
                <span className="text-(--color-th-teal)">{icon}</span>
                {title}
            </h2>
            {children}
        </section>
    );
}

function Row({ label, value }: { label: string; value: React.ReactNode }) {
    return (
        <div className="flex items-center justify-between gap-4 border-b border-border/60 py-2 last:border-0">
            <span className="text-sm text-(--color-muted-foreground)">{label}</span>
            <span className="truncate font-mono text-xs text-foreground/80">{value}</span>
        </div>
    );
}

/** Narc's safety-judge config (mirrors the daemon's `/api/judge` reply). */
interface JudgeState {
    enabled: boolean;
    strictness: 'lenient' | 'normal' | 'strict';
    model: string;
}

const STRICTNESS: { id: JudgeState['strictness']; label: string; hint: string }[] = [
    { id: 'lenient', label: 'Lenient', hint: 'Only hard signals (secret exfil, delete tools) are judged.' },
    { id: 'normal', label: 'Normal', hint: 'Ambiguous hits also go to the judge; the default.' },
    { id: 'strict', label: 'Strict', hint: 'Fails closed — blocks ambiguous hits when the judge is unreachable.' },
];

/** The safety-judge controls (pearls th-eec7a5, th-7aa2af). Enable/disable the
 * LLM judge, pick how strict it is, and pick the model it runs as (independent
 * of the chat model). Disabling only turns off the LLM escalation — the
 * hard-signal detectors and the permission circuit-breakers still run. */
function JudgeSection() {
    const { http, token } = resolveTarget();
    const authHeaders: Record<string, string> = token ? { authorization: `Bearer ${token}` } : {};
    const [state, setState] = useState<JudgeState | null>(null);
    const [busy, setBusy] = useState(false);

    useEffect(() => {
        let live = true;
        fetch(`${http}/api/judge`, { headers: authHeaders })
            .then((r) => (r.ok ? (r.json() as Promise<JudgeState>) : null))
            .then((j) => {
                if (live && j) setState(j);
            })
            .catch(() => {});
        return () => {
            live = false;
        };
        // eslint-disable-next-line react-hooks/exhaustive-deps
    }, [http]);

    const patch = useCallback(
        async (body: Partial<JudgeState>) => {
            setBusy(true);
            try {
                const r = await fetch(`${http}/api/judge`, {
                    method: 'POST',
                    headers: { 'content-type': 'application/json', ...authHeaders },
                    body: JSON.stringify(body),
                });
                if (r.ok) setState((await r.json()) as JudgeState);
            } finally {
                setBusy(false);
            }
        },
        // eslint-disable-next-line react-hooks/exhaustive-deps
        [http],
    );

    if (!state) return <p className="text-sm text-(--color-muted-foreground)">Checking…</p>;

    // The models the judge can run as: the mode lineup, plus whatever is set now
    // (so a custom/fast model that isn't a Smooth Mode still shows selected).
    const models = Array.from(new Set([state.model, ...MODES.map((m) => m.model)]));

    return (
        <div className="space-y-4">
            <div className="flex items-center justify-between gap-4 rounded-2xl border border-border bg-panel/60 px-4 py-3">
                <div className="min-w-0">
                    <div className="text-sm text-foreground">LLM safety judge</div>
                    <div className="text-xs text-(--color-muted-foreground)">
                        {state.enabled
                            ? 'On — ambiguous tool calls are adjudicated before they run.'
                            : 'Off — detectors and circuit-breakers still block clear threats; no LLM second opinion.'}
                    </div>
                </div>
                <button
                    type="button"
                    disabled={busy}
                    onClick={() => void patch({ enabled: !state.enabled })}
                    role="switch"
                    aria-checked={state.enabled}
                    className={`relative h-6 w-11 shrink-0 rounded-full transition disabled:opacity-60 ${
                        state.enabled ? 'bg-(--color-th-teal)' : 'bg-(--color-muted-foreground)/30'
                    }`}
                >
                    <span className={`absolute top-0.5 size-5 rounded-full bg-white transition ${state.enabled ? 'left-[22px]' : 'left-0.5'}`} />
                </button>
            </div>

            <div className={state.enabled ? '' : 'pointer-events-none opacity-50'}>
                <div className="mb-1 text-xs uppercase tracking-wide text-(--color-muted-foreground)">Strictness</div>
                <div className="grid grid-cols-3 gap-2">
                    {STRICTNESS.map((s) => (
                        <button
                            key={s.id}
                            type="button"
                            disabled={busy || !state.enabled}
                            title={s.hint}
                            onClick={() => void patch({ strictness: s.id })}
                            className={`rounded-xl border px-3 py-2 text-sm transition ${
                                s.id === state.strictness
                                    ? 'border-transparent bg-(--color-th-teal)/12 text-foreground ring-1 ring-(--color-th-teal)/40'
                                    : 'border-border bg-panel/60 text-foreground/80 hover:bg-panel-2'
                            }`}
                        >
                            {s.label}
                        </button>
                    ))}
                </div>
                <p className="mt-2 text-xs text-(--color-muted-foreground)">{STRICTNESS.find((s) => s.id === state.strictness)?.hint}</p>

                <div className="mt-4 mb-1 text-xs uppercase tracking-wide text-(--color-muted-foreground)">Judge model</div>
                <select
                    value={state.model}
                    disabled={busy || !state.enabled}
                    onChange={(e) => void patch({ model: e.target.value })}
                    className="w-full rounded-xl border border-border bg-panel/60 px-3 py-2 font-mono text-xs text-foreground/90"
                >
                    {models.map((m) => (
                        <option key={m} value={m}>
                            {m}
                        </option>
                    ))}
                </select>
                <p className="mt-2 text-xs text-(--color-muted-foreground)">The judge is a cheap classifier; a fast model is fine and keeps it snappy.</p>
            </div>
        </div>
    );
}

export default function SettingsPage({
    mode,
    setMode,
    modelCosts,
    status,
    push,
}: {
    mode: SmoothMode;
    setMode: (id: string) => void;
    modelCosts: ModelCosts;
    status: Status;
    push: PushApi;
}) {
    const { http, token } = resolveTarget();

    return (
        <main className="flex min-h-0 flex-1 flex-col overflow-y-auto py-6">
            <h1 className="font-display text-xl text-foreground">Settings</h1>

            <Section icon={<KeyRound className="size-4" />} title="Model">
                <p className="mb-3 text-xs text-(--color-muted-foreground)">
                    Each turn is pinned to one model. Currently: {mode.emoji} {mode.model}.
                </p>
                <div className="overflow-hidden rounded-2xl border border-border bg-panel/60">
                    <ModelPicker current={mode.model} onPick={setMode} costs={modelCosts} />
                </div>
            </Section>

            <Section icon={<Scale className="size-4" />} title="Safety judge">
                <JudgeSection />
            </Section>

            <Section icon={<Bell className="size-4" />} title="Notifications">
                {!push.supported ? (
                    <p className="text-sm text-(--color-muted-foreground)">Web push isn’t available in this browser.</p>
                ) : push.enabled ? (
                    <p className="text-sm text-(--color-online)">Notifications are on for this device.</p>
                ) : push.configured === false ? (
                    <p className="text-sm text-(--color-muted-foreground)">
                        Push notifications aren’t set up on Big Smooth yet — the daemon has no VAPID keys, so there’s nothing to enable. (Tracked:
                        auto-provision on first run.)
                    </p>
                ) : push.configured === null ? (
                    <p className="text-sm text-(--color-muted-foreground)">Checking…</p>
                ) : (
                    <button
                        type="button"
                        onClick={() => void push.enable()}
                        disabled={push.busy}
                        className="rounded-full bg-coral px-4 py-1.5 text-sm text-(--color-coral-ink) disabled:opacity-60"
                    >
                        {push.busy ? 'Enabling…' : 'Enable notifications'}
                    </button>
                )}
            </Section>

            <Section icon={<Link2 className="size-4" />} title="Connection">
                <ConnectionSwitcher />
                <div className="rounded-2xl border border-border bg-panel/60 px-4">
                    <Row label="Daemon" value={http || '—'} />
                    <Row label="Auth token" value={token ? 'present' : 'none'} />
                    <Row label="Signed in as" value={status.identity ?? 'not signed in'} />
                    <Row label="Model in use" value={status.model ?? '—'} />
                </div>
            </Section>

            <Section icon={<ShieldCheck className="size-4" />} title="Access (macOS)">
                <p className="text-sm text-(--color-muted-foreground)">
                    Calendar, Reminders, Messages, and Full Disk Access are macOS permission grants. Manage them from the Big Smooth menu-bar icon →{' '}
                    <span className="text-foreground/80">Set Up</span> — they can’t be granted from this page.
                </p>
            </Section>
        </main>
    );
}

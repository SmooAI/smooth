//! The Settings tab — the controls that were previously only reachable through
//! the composer slash-menu / localStorage, plus connection + access info.
//!
//! Presence styling: teal marks the *active* choice (a live selection is the one
//! "present" thing on the page); amber is never used — settings never demand you.

import { Bell, KeyRound, Link2, ShieldCheck } from 'lucide-react';
import { useCallback, useEffect, useState } from 'react';

import { ModelPicker } from './ModelSelector';
import { type ModelCosts, type SmoothMode } from './modes';
import { resolveTarget, type Status } from './operator';

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

interface PushApi {
    supported: boolean;
    enabled: boolean;
    busy: boolean;
    /** Whether the daemon has push (VAPID) configured. null = still checking. */
    configured: boolean | null;
    enable: () => void | Promise<void>;
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

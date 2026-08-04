//! The Settings tab — the controls that were previously only reachable through
//! the composer slash-menu / localStorage, plus connection + access info.
//!
//! Presence styling: teal marks the *active* choice (a live selection is the one
//! "present" thing on the page); amber is never used — settings never demand you.

import { Bell, KeyRound, Link2, ShieldCheck } from 'lucide-react';

import { MODES, type SmoothMode } from './modes';
import { resolveTarget, type Status } from './operator';

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

function ModeButton({ m, active, onPick }: { m: SmoothMode; active: boolean; onPick: () => void }) {
    return (
        <button
            type="button"
            onClick={onPick}
            title={m.model}
            className={`flex items-center gap-1.5 rounded-xl border px-3 py-2 text-left text-sm transition ${
                active
                    ? 'border-transparent bg-(--color-th-teal)/12 ring-1 ring-(--color-th-teal)/40 text-foreground'
                    : 'border-border bg-panel/60 text-foreground/80 hover:bg-panel-2'
            }`}
        >
            <span aria-hidden>{m.emoji}</span>
            <span className="min-w-0">
                <span className="block truncate">{m.label}</span>
                <span className="block truncate font-mono text-[10px] text-(--color-muted-foreground)">{m.model}</span>
            </span>
        </button>
    );
}

export default function SettingsPage({ mode, setMode, status, push }: { mode: SmoothMode; setMode: (id: string) => void; status: Status; push: PushApi }) {
    const { http, token } = resolveTarget();
    const budget = MODES.filter((m) => m.tier === 'budget');
    const premium = MODES.filter((m) => m.tier === 'premium');

    return (
        <main className="flex min-h-0 flex-1 flex-col overflow-y-auto py-6">
            <h1 className="font-display text-xl text-foreground">Settings</h1>

            <Section icon={<KeyRound className="size-4" />} title="Model">
                <p className="mb-3 text-xs text-(--color-muted-foreground)">
                    Smooth Mode pins each turn to a model. Currently: {mode.emoji} {mode.label}.
                </p>
                <div className="mb-1 text-xs uppercase tracking-wide text-(--color-muted-foreground)">Budget</div>
                <div className="grid grid-cols-2 gap-2 sm:grid-cols-3">
                    {budget.map((m) => (
                        <ModeButton key={m.id} m={m} active={m.id === mode.id} onPick={() => setMode(m.id)} />
                    ))}
                </div>
                <div className="mt-3 mb-1 text-xs uppercase tracking-wide text-(--color-muted-foreground)">Premium</div>
                <div className="grid grid-cols-2 gap-2 sm:grid-cols-3">
                    {premium.map((m) => (
                        <ModeButton key={m.id} m={m} active={m.id === mode.id} onPick={() => setMode(m.id)} />
                    ))}
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

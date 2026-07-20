// "Sign in with Smoo" — a tasteful top-right affordance that logs the `th` CLI
// into Smoo AI via the daemon's OAuth2 Device Authorization Grant (RFC 8628,
// th-ea7b54). No redirect_uri needed, so it works for a browser on the tailnet.
// The daemon polls smoo.ai in the background; this UI shows the user code +
// approval link and watches /api/auth/status (th-bc624a) flip to logged-in.
// Same-origin: the daemon serves this SPA and owns /auth/* + /api/auth/status.

import { ExternalLink, Sparkles, UserCheck } from 'lucide-react';
import { useEffect, useState } from 'react';

interface AuthStatus {
    loggedIn: boolean;
    user: string | null;
    orgId: string | null;
    // th-cbf613: loggedIn is now expiry-aware. `expired` means a session is on
    // disk but dead — the daemon's heartbeat couldn't renew it and a human has
    // to sign in again.
    expired?: boolean;
}

interface DeviceStart {
    user_code: string;
    verification_uri: string;
    verification_uri_complete: string;
}

// top offset clears the iOS status bar in an installed PWA (th-086d97).
const PILL = 'fixed top-[calc(env(safe-area-inset-top)+1rem)] right-4 z-10 flex items-center gap-1.5 rounded-full bg-panel/80 px-3 py-1.5 text-xs';

export function SmooSignIn() {
    const [status, setStatus] = useState<AuthStatus | null>(null);
    const [device, setDevice] = useState<DeviceStart | null>(null);
    const [error, setError] = useState(false);

    // Poll status: the daemon's background poll flips this to logged-in once the
    // user approves. 5s is plenty for a human flow and idle-cheap. Never throws —
    // a failed fetch just leaves the last known state.
    useEffect(() => {
        let alive = true;
        const load = () =>
            fetch('/api/auth/status')
                .then((r) => (r.ok ? r.json() : null))
                .then((s: AuthStatus | null) => {
                    if (alive && s) setStatus(s);
                })
                .catch(() => {});
        load();
        const id = setInterval(load, 5000);
        return () => {
            alive = false;
            clearInterval(id);
        };
    }, []);

    const startSignIn = () => {
        setError(false);
        fetch('/auth/device/start', { method: 'POST' })
            .then((r) => (r.ok ? r.json() : Promise.reject(new Error('start failed'))))
            .then((d: DeviceStart) => {
                setDevice(d);
                // Pop the approval page for them; if the popup is blocked they can
                // still click the link in the pill.
                window.open(d.verification_uri_complete, '_blank', 'noopener');
            })
            .catch(() => setError(true));
    };

    // Until the first poll returns, render nothing rather than flash a wrong state.
    if (!status) return null;

    if (status.loggedIn) {
        return (
            <div
                className={`${PILL} text-(--color-muted-foreground) opacity-70`}
                title={status.orgId ? `Signed in to org ${status.orgId}` : 'Signed in to Smoo AI'}
            >
                <UserCheck className="size-3.5 text-(--color-online)" /> {status.user ?? 'Signed in'}
            </div>
        );
    }

    // Waiting for approval: show the user code + a link to the approval page.
    if (device && !error) {
        return (
            <a
                href={device.verification_uri_complete}
                target="_blank"
                rel="noopener noreferrer"
                title="Open the Smoo AI approval page, then enter this code"
                className={`${PILL} text-(--color-muted-foreground) transition hover:opacity-100`}
            >
                <Sparkles className="size-3.5 animate-pulse text-(--color-online)" />
                <span>Enter code</span>
                <code className="rounded bg-black/20 px-1.5 py-0.5 font-mono tracking-widest text-(--color-foreground)">{device.user_code}</code>
                <ExternalLink className="size-3" />
            </a>
        );
    }

    return (
        <button
            onClick={startSignIn}
            title={
                status.expired
                    ? `Your Smoo AI session${status.user ? ` (${status.user})` : ''} expired and could not be renewed — sign in again`
                    : 'Sign in with Smoo AI so Big Smooth can act on your org'
            }
            className={`${PILL} text-(--color-muted-foreground) opacity-70 transition hover:opacity-100`}
        >
            <Sparkles className="size-3.5" /> {error ? 'Sign-in failed — retry' : status.expired ? 'Session expired — sign in' : 'Sign in with Smoo'}
        </button>
    );
}

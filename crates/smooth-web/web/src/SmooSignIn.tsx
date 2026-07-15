// "Sign in with Smoo" — a tasteful top-right affordance that logs the `th` CLI
// into Smoo AI via the daemon's browser OAuth2 + PKCE flow (th-bc624a). Once
// signed in, Big Smooth can act on the user's Smoo org through th-backed
// extensions. Same-origin: the daemon serves this SPA and owns /auth/* +
// /api/auth/status.

import { Sparkles, UserCheck } from 'lucide-react';
import { useEffect, useState } from 'react';

interface AuthStatus {
    loggedIn: boolean;
    user: string | null;
    orgId: string | null;
}

export function SmooSignIn() {
    const [status, setStatus] = useState<AuthStatus | null>(null);

    // Poll status: the sign-in completes in a separate tab (the OAuth callback),
    // so we watch for it to flip to logged-in. 5s is plenty for a human flow and
    // idle-cheap. Never throws — a failed fetch just leaves the last known state.
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

    // Until the first poll returns, render nothing rather than flash a wrong state.
    if (!status) return null;

    if (status.loggedIn) {
        return (
            <div
                className="fixed top-4 right-4 z-10 flex items-center gap-1.5 rounded-full bg-panel/80 px-3 py-1.5 text-xs text-(--color-muted-foreground) opacity-70"
                title={status.orgId ? `Signed in to org ${status.orgId}` : 'Signed in to Smoo AI'}
            >
                <UserCheck className="size-3.5 text-(--color-online)" /> {status.user ?? 'Signed in'}
            </div>
        );
    }

    return (
        <button
            onClick={() => {
                window.location.href = '/auth/login';
            }}
            title="Sign in with Smoo AI so Big Smooth can act on your org"
            className="fixed top-4 right-4 z-10 flex items-center gap-1.5 rounded-full bg-panel/80 px-3 py-1.5 text-xs text-(--color-muted-foreground) opacity-70 transition hover:opacity-100"
        >
            <Sparkles className="size-3.5" /> Sign in with Smoo
        </button>
    );
}

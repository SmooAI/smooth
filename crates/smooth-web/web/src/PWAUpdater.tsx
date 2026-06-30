// Keep the installed PWA current. With `registerType: 'prompt'` the new service
// worker waits instead of silently swapping; we poll for updates while the app
// is open and, when one lands, force a refresh through a modal the user can't
// dismiss — so a long-lived Big Smooth tab never drifts onto stale code.

import { useRegisterSW } from 'virtual:pwa-register/react';

/** How often an open tab checks for a newer deploy. */
const UPDATE_POLL_MS = 60_000;

export function PWAUpdater() {
    const {
        needRefresh: [needRefresh],
        updateServiceWorker,
    } = useRegisterSW({
        onRegisteredSW(_swUrl, registration) {
            if (registration) {
                setInterval(() => void registration.update(), UPDATE_POLL_MS);
            }
        },
    });

    if (!needRefresh) return null;

    return (
        <div className="fixed inset-0 z-50 grid place-items-center bg-background/85 p-6 backdrop-blur">
            <div className="needs-you w-full max-w-sm rounded-2xl bg-panel/95 p-6 text-center shadow-2xl">
                <img src="/smooth-icon.svg" alt="Smooth" className="mx-auto mb-3 size-10" />
                <h2 className="greeting text-xl text-foreground">A fresh Big Smooth is ready</h2>
                <p className="mt-1.5 text-sm text-(--color-muted-foreground)">A new version just shipped. Refresh to pick it up — takes a second.</p>
                <button
                    onClick={() => void updateServiceWorker(true)}
                    className="mt-4 inline-flex w-full items-center justify-center rounded-full bg-coral px-5 py-2.5 text-sm font-semibold text-(--color-coral-ink) transition hover:brightness-110"
                >
                    Refresh now
                </button>
            </div>
        </div>
    );
}

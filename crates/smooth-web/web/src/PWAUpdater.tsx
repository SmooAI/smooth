// Keep the installed PWA current. With `registerType: 'prompt'` the new service
// worker waits instead of silently swapping; we poll for updates while the app
// is open and, when one lands, force a refresh through a modal the user can't
// dismiss — so a long-lived Big Smooth tab never drifts onto stale code.
//
// EXCEPT in the Electron desktop app (th-003dc7): there the SPA is served by the
// bundled daemon and the app updates via its OWN OTA (electron-updater). A
// service worker adds nothing there — it only produces a SECOND "refresh" prompt
// on top of the app's "restart" (the double update Brent saw) and a stale SPA
// cache after an OTA. Push on desktop is native (`window.bigSmooth.notify`), not
// the SW, so nothing depends on it. So desktop runs NO SW updater and tears down
// any SW/caches a prior build left behind; browser + installed-PWA users keep the
// full flow below.

import { useEffect, useState } from 'react';
import { useRegisterSW } from 'virtual:pwa-register/react';

/** True inside the Electron desktop shell — its preload exposes `window.bigSmooth`
 * (the native bridge). The web/mobile PWA has no such global. */
function isDesktopApp(): boolean {
    return typeof window !== 'undefined' && !!(window as unknown as { bigSmooth?: unknown }).bigSmooth;
}

/** Unregister every service worker and clear its caches. Used on desktop to shed
 * an SW a prior build registered (and that vite-plugin-pwa's auto-register puts
 * back each load) — so the Electron webview always loads fresh from the local
 * daemon and never shows the PWA refresh prompt. Best-effort; never throws. */
async function tearDownServiceWorkers(): Promise<void> {
    try {
        if ('serviceWorker' in navigator) {
            const regs = await navigator.serviceWorker.getRegistrations();
            await Promise.all(regs.map((r) => r.unregister().catch(() => false)));
        }
        if ('caches' in window) {
            const keys = await caches.keys();
            await Promise.all(keys.map((k) => caches.delete(k).catch(() => false)));
        }
    } catch {
        // best-effort — a residual SW is harmless, just don't crash the shell
    }
}

/** How often an open tab checks for a newer deploy. */
const UPDATE_POLL_MS = 60_000;

/** A bulletproof "get the latest now" — `updateServiceWorker(true)` relies on
 * the SW `controllerchange` event to reload, which iOS Safari fires
 * unreliably. So we also unregister every service worker and clear the caches,
 * then hard-reload: the navigation refetches from the network no matter what,
 * and the SW re-registers fresh on the next load. */
async function forceRefresh(updateServiceWorker: (reload?: boolean) => Promise<void>) {
    try {
        await updateServiceWorker(true).catch(() => {});
        if ('serviceWorker' in navigator) {
            const regs = await navigator.serviceWorker.getRegistrations();
            await Promise.all(regs.map((r) => r.unregister().catch(() => false)));
        }
        if ('caches' in window) {
            const keys = await caches.keys();
            await Promise.all(keys.map((k) => caches.delete(k).catch(() => false)));
        }
    } finally {
        window.location.reload();
    }
}

export function PWAUpdater() {
    // A hook-free switch so each branch's hooks stay unconditional (React rules):
    // desktop never touches the SW updater; the browser/PWA path is unchanged.
    if (isDesktopApp()) return <DesktopServiceWorkerTeardown />;
    return <BrowserPWAUpdater />;
}

/** Desktop: shed any service worker + caches, show nothing. No `useRegisterSW`,
 * so no refresh prompt — the app's OTA is the single update path. */
function DesktopServiceWorkerTeardown() {
    useEffect(() => {
        void tearDownServiceWorkers();
    }, []);
    return null;
}

function BrowserPWAUpdater() {
    const [refreshing, setRefreshing] = useState(false);
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
                    onClick={() => {
                        setRefreshing(true);
                        void forceRefresh(updateServiceWorker);
                    }}
                    disabled={refreshing}
                    className="mt-4 inline-flex w-full items-center justify-center rounded-full bg-coral px-5 py-2.5 text-sm font-semibold text-(--color-coral-ink) transition hover:brightness-110 disabled:opacity-60"
                >
                    {refreshing ? 'Refreshing…' : 'Refresh now'}
                </button>
            </div>
        </div>
    );
}

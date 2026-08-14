// Notification enrollment for this device. Two paths:
//
//  - **Browser PWA** (phone / desktop Chrome added to home screen): Web Push —
//    ask permission, subscribe with the daemon's VAPID key, register it so Big
//    Smooth can push with the tab closed. Same-origin (relative URLs).
//    iOS note: Web Push only works for a PWA *added to the home screen* (16.4+).
//
//  - **Electron desktop** (`window.bigSmooth.notify` present): the webview can't
//    do Web Push — Chromium ships no push service, so `pushManager.subscribe()`
//    rejects and the button silently no-ops (th-6af4b1). We fall back to native
//    OS notifications over the preload IPC bridge instead: `enable()` fires a
//    confirmation notification, and `notify()` relays finished replies.

import { useCallback, useEffect, useState } from 'react';

function urlBase64ToBytes(base64: string): Uint8Array<ArrayBuffer> {
    const padding = '='.repeat((4 - (base64.length % 4)) % 4);
    const b64 = (base64 + padding).replace(/-/g, '+').replace(/_/g, '/');
    const raw = atob(b64);
    const bytes = new Uint8Array(new ArrayBuffer(raw.length));
    for (let i = 0; i < raw.length; i++) bytes[i] = raw.charCodeAt(i);
    return bytes;
}

function authHeaders(): Record<string, string> {
    const token = (window as unknown as { __SMOOTH_TOKEN__?: string }).__SMOOTH_TOKEN__;
    return token ? { Authorization: `Bearer ${token}` } : {};
}

/** The Electron native-notification bridge, if this is the desktop app. */
function nativeBridge(): ((p: { title?: string; body?: string; deepLink?: string }) => Promise<boolean>) | undefined {
    const notify = typeof window === 'undefined' ? undefined : window.bigSmooth?.notify;
    return typeof notify === 'function' ? notify.bind(window.bigSmooth) : undefined;
}

const NATIVE_ENABLED_KEY = 'smooth.push.native';

export interface PushApi {
    supported: boolean;
    enabled: boolean;
    busy: boolean;
    /** Whether the daemon has push (VAPID) configured — always true on the native
     * (Electron) path, which needs no daemon keys. null = still checking. */
    configured: boolean | null;
    enable: () => void | Promise<void>;
    /** Relay a notification to this device — native OS notification on desktop, a
     * no-op in the browser (where the daemon pushes via the service worker). */
    notify: (title: string, body: string) => void;
}

export function usePush(): PushApi {
    const native = nativeBridge();
    const supported = !!native || (typeof window !== 'undefined' && 'serviceWorker' in navigator && 'PushManager' in window && 'Notification' in window);
    const [enabled, setEnabled] = useState(() => !!native && localStorage.getItem(NATIVE_ENABLED_KEY) === '1');
    const [busy, setBusy] = useState(false);
    // Whether the DAEMON has push set up (VAPID keys). null = still checking.
    // Native path needs no daemon keys, so it's configured unconditionally.
    const [configured, setConfigured] = useState<boolean | null>(native ? true : null);

    useEffect(() => {
        if (native || !supported) {
            return;
        }
        navigator.serviceWorker.ready
            .then((reg) => reg.pushManager.getSubscription())
            .then((sub) => setEnabled(!!sub))
            .catch(() => {});
        fetch('/push/key', { headers: authHeaders() })
            .then((r) => setConfigured(r.ok))
            .catch(() => setConfigured(false));
    }, [supported, native]);

    const notify = useCallback(
        (title: string, body: string) => {
            if (native) void native({ title, body }).catch(() => {});
        },
        [native],
    );

    const enable = useCallback(async () => {
        if (busy) return;
        setBusy(true);
        try {
            if (native) {
                // Native path: firing a confirmation notification IS the enrollment
                // and the "it works" test. If it shows, notifications are on.
                const ok = await native({ title: 'Big Smooth', body: 'Notifications are on for this Mac.' });
                if (ok) {
                    localStorage.setItem(NATIVE_ENABLED_KEY, '1');
                    setEnabled(true);
                }
                return;
            }
            if (!supported) return;
            if ((await Notification.requestPermission()) !== 'granted') return;
            const keyRes = await fetch('/push/key', { headers: authHeaders() });
            if (!keyRes.ok) return; // push not configured on the daemon (no VAPID keys)
            const { publicKey } = await keyRes.json();
            const reg = await navigator.serviceWorker.ready;
            const sub = await reg.pushManager.subscribe({
                userVisibleOnly: true,
                applicationServerKey: urlBase64ToBytes(publicKey),
            });
            await fetch('/push/subscribe', {
                method: 'POST',
                headers: { 'Content-Type': 'application/json', ...authHeaders() },
                body: JSON.stringify(sub.toJSON()),
            });
            setEnabled(true);
        } finally {
            setBusy(false);
        }
    }, [supported, busy, native]);

    return { supported, enabled, busy, configured, enable, notify };
}

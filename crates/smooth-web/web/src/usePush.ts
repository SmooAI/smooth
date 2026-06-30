// Web Push enrollment: ask permission, subscribe with the daemon's VAPID key, and
// register the subscription so Big Smooth can push to this device (an installed PWA)
// with the tab closed. Same-origin (relative URLs) — the daemon serves this SPA.
//
// iOS note: Web Push only works for a PWA *added to the home screen* (iOS 16.4+).

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

export function usePush() {
    const supported = typeof window !== 'undefined' && 'serviceWorker' in navigator && 'PushManager' in window && 'Notification' in window;
    const [enabled, setEnabled] = useState(false);
    const [busy, setBusy] = useState(false);

    useEffect(() => {
        if (!supported) return;
        navigator.serviceWorker.ready
            .then((reg) => reg.pushManager.getSubscription())
            .then((sub) => setEnabled(!!sub))
            .catch(() => {});
    }, [supported]);

    const enable = useCallback(async () => {
        if (!supported || busy) return;
        setBusy(true);
        try {
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
    }, [supported, busy]);

    return { supported, enabled, busy, enable };
}

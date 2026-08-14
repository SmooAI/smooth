//! Native OS notifications for the desktop app (th-6af4b1).
//!
//! The Electron webview can't do Web Push — Chromium ships no push service, so
//! `PushManager.subscribe()` rejects and the SPA's "Enable notifications" button
//! silently no-ops. Instead the renderer (`usePush`) posts `{title, body}` over
//! this IPC bridge and the main process shows a real native notification. Pure
//! payload normalization lives here so it's unit-testable without Electron.

export interface NotifyPayload {
    title?: unknown;
    body?: unknown;
    deepLink?: unknown;
}

export interface Notification {
    title: string;
    body: string;
}

/** Normalize an untrusted IPC payload into a showable notification, or `null`
 * when there's nothing worth showing (neither title nor body). The renderer is
 * our own SPA, but the payload is still validated + clamped to lock-screen
 * lengths. */
export function buildNotification(p: NotifyPayload): Notification | null {
    const title = typeof p.title === 'string' ? p.title.trim() : '';
    const body = typeof p.body === 'string' ? p.body.trim() : '';
    if (!title && !body) return null;
    return { title: (title || 'Big Smooth').slice(0, 120), body: body.slice(0, 240) };
}

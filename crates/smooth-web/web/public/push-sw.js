// Imported into the generated service worker (vite.config → workbox.importScripts).
// Wakes on a Web Push from the daemon and shows the notification; focuses the app
// (or opens it) when the notification is tapped. This is what lets Big Smooth reach
// the phone with the PWA closed.

self.addEventListener('push', (event) => {
    let data = { title: 'Big Smooth', body: 'You have an update.' };
    try {
        if (event.data) data = { ...data, ...event.data.json() };
    } catch {
        // non-JSON / empty payload — keep the default
    }
    event.waitUntil(
        self.registration.showNotification(data.title, {
            body: data.body,
            icon: '/pwa-192x192.png',
            badge: '/favicon-32x32.png',
            tag: data.tag || 'big-smooth',
        }),
    );
});

self.addEventListener('notificationclick', (event) => {
    event.notification.close();
    event.waitUntil(
        (async () => {
            const clients = await self.clients.matchAll({ type: 'window', includeUncontrolled: true });
            const open = clients.find((c) => 'focus' in c);
            if (open) return open.focus();
            return self.clients.openWindow('/');
        })(),
    );
});

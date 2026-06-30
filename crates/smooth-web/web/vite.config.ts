import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';
import tailwindcss from '@tailwindcss/vite';
import { VitePWA } from 'vite-plugin-pwa';

export default defineConfig({
    plugins: [
        react(),
        tailwindcss(),
        VitePWA({
            // `prompt`, not `autoUpdate`: a new service worker waits until the user
            // accepts the forced-refresh modal (src/PWAUpdater.tsx), so we never
            // swap code out from under a live session silently.
            registerType: 'prompt',
            includeAssets: [
                'favicon.ico',
                'favicon.png',
                'favicon-32x32.png',
                'favicon-16x16.png',
                'apple-touch-icon.png',
                'apple-touch-icon-167.png',
                'apple-touch-icon-152.png',
                'apple-touch-icon-120.png',
                'smooth-icon.svg',
                'logo.svg',
            ],
            manifest: {
                name: 'Big Smooth — your always-on AI',
                short_name: 'Big Smooth',
                description: 'Your always-on personal AI operator.',
                theme_color: '#0a0a0a',
                background_color: '#0a0a0a',
                display: 'standalone',
                start_url: '/',
                scope: '/',
                icons: [
                    { src: '/pwa-192x192.png', sizes: '192x192', type: 'image/png' },
                    { src: '/pwa-512x512.png', sizes: '512x512', type: 'image/png' },
                    { src: '/pwa-512x512.png', sizes: '512x512', type: 'image/png', purpose: 'any maskable' },
                    { src: '/smooth-icon.svg', type: 'image/svg+xml', purpose: 'any' },
                ],
            },
            workbox: {
                // The daemon serves these — the SW must never shadow them with the
                // SPA shell. (th-c89c2a: /admin/* and /search added alongside the API.)
                navigateFallbackDenylist: [/^\/api/, /^\/admin/, /^\/search/, /^\/push/, /^\/health/, /^\/ws/],
                // Pull in the Web Push handler (public/push-sw.js) so the SW shows
                // notifications Big Smooth pushes to the phone (src/usePush.ts).
                importScripts: ['push-sw.js'],
            },
            devOptions: {
                enabled: false,
            },
        }),
    ],
    server: {
        port: 3100,
        proxy: {
            '/api': 'http://localhost:4400',
            '/health': 'http://localhost:4400',
            '/ws': {
                target: 'ws://localhost:4400',
                ws: true,
            },
        },
    },
    build: {
        outDir: 'dist',
        emptyOutDir: true,
    },
});

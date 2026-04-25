import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';
import tailwindcss from '@tailwindcss/vite';
import { VitePWA } from 'vite-plugin-pwa';

export default defineConfig({
    plugins: [
        react(),
        tailwindcss(),
        VitePWA({
            registerType: 'autoUpdate',
            includeAssets: [
                'favicon.ico',
                'favicon.png',
                'favicon-32x32.png',
                'favicon-16x16.png',
                'apple-touch-icon.png',
                'apple-touch-icon-167.png',
                'apple-touch-icon-152.png',
                'apple-touch-icon-120.png',
                'logo.svg',
            ],
            manifest: {
                name: 'Smooth — Smoo AI Agent Orchestration',
                short_name: 'Smooth',
                description: 'Smoo AI agent orchestration dashboard',
                theme_color: '#0a0a0a',
                background_color: '#0a0a0a',
                display: 'standalone',
                start_url: '/',
                scope: '/',
                icons: [
                    { src: '/pwa-192x192.png', sizes: '192x192', type: 'image/png' },
                    { src: '/pwa-512x512.png', sizes: '512x512', type: 'image/png' },
                    { src: '/pwa-512x512.png', sizes: '512x512', type: 'image/png', purpose: 'any maskable' },
                ],
            },
            workbox: {
                navigateFallbackDenylist: [/^\/api/, /^\/health/, /^\/ws/],
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

import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';
import tailwindcss from '@tailwindcss/vite';

export default defineConfig({
    plugins: [react(), tailwindcss()],
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

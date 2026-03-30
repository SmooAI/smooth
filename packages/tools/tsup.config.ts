import { defineConfig } from 'tsup';

export default defineConfig({
    entry: ['src/index.ts', 'src/mcp-server.ts'],
    format: ['esm'],
    dts: true,
    sourcemap: true,
    clean: true,
    banner: {
        js: '#!/usr/bin/env node',
    },
});

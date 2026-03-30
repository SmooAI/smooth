import { defineConfig } from 'tsup';

export default defineConfig({
    entry: ['src/index.tsx'],
    format: ['esm'],
    dts: true,
    sourcemap: true,
    clean: true,
    banner: {
        js: '#!/usr/bin/env node',
    },
    external: ['react', 'ink'],
});

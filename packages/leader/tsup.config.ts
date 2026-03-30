import { defineConfig } from 'tsup';

export default defineConfig({
    entry: ['src/server.ts'],
    format: ['esm'],
    dts: true,
    sourcemap: true,
    clean: true,
});

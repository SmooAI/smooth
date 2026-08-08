// Copy this host's `smooth-daemon` AND `th` into resources/current/ so
// electron-builder can bundle both. Reuses the app's own resolution logic.
//
// ponytail: stages the HOST's binaries only — each OS builds its own installer, so
// one directory is enough. A cross-compiling CI job drops its target's binaries
// into the same place before calling electron-builder.

import { copyFileSync, mkdirSync } from 'node:fs';
import { join } from 'node:path';

import { resolveDaemonBin, resolveThBin } from '../dist/daemon.js';

const dir = join(import.meta.dirname, '..', 'resources', 'current');
mkdirSync(dir, { recursive: true });

const exe = process.platform === 'win32' ? '.exe' : '';

stage('smooth-daemon', resolveDaemonBin(), `smooth-daemon${exe}`);
stage('th', resolveThBin(), `th${exe}`);

function stage(label, src, filename) {
    if (!src) {
        console.error(`stage-daemon: no ${label} found. Build it first: \`pnpm install:th\` from the repo root.`);
        process.exit(1);
    }
    const dest = join(dir, filename);
    copyFileSync(src, dest);
    console.log(`staged ${src} → ${dest}`);
}

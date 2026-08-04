// Copy this host's `smooth-daemon` into resources/<platform>/ so electron-builder
// can bundle it. Reuses the app's own resolution logic — one lookup, not two.
//
// ponytail: stages the HOST's binary only — each OS builds its own installer, so
// one directory is enough. A cross-compiling CI job would drop its target's binary
// into the same place before calling electron-builder.

import { copyFileSync, mkdirSync } from 'node:fs';
import { join } from 'node:path';

import { resolveDaemonBin } from '../dist/daemon.js';

const bin = resolveDaemonBin();
if (!bin) {
    console.error('stage-daemon: no smooth-daemon found. Build it first: `pnpm install:th` from the repo root.');
    process.exit(1);
}

const dir = join(import.meta.dirname, '..', 'resources', 'current');
mkdirSync(dir, { recursive: true });
const dest = join(dir, process.platform === 'win32' ? 'smooth-daemon.exe' : 'smooth-daemon');
copyFileSync(bin, dest);
console.log(`staged ${bin} → ${dest}`);

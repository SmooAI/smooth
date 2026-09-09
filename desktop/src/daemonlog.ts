//! `~/Library/Logs/Big Smooth/daemon.log` — a small size-rotating append log
//! for the daemon child's stdout/stderr and the supervisor's own lines. No
//! electron import (so `node --test` can load it); the path is computed here
//! rather than via `app.getPath('logs')` for the same reason.

import { closeSync, existsSync, mkdirSync, openSync, renameSync, statSync, unlinkSync, writeSync } from 'node:fs';
import { homedir } from 'node:os';
import { dirname, join } from 'node:path';

/** Where the desktop app keeps the daemon logs. macOS: the Console.app-visible
 * `~/Library/Logs/<app>`; elsewhere `~/.smooth/logs`. `SMOOTH_DESKTOP_LOG_DIR`
 * overrides (tests, and a second app instance that must not share a file). */
export function daemonLogDir(env: NodeJS.ProcessEnv = process.env, platform: NodeJS.Platform = process.platform, home: string = homedir()): string {
    const override = (env.SMOOTH_DESKTOP_LOG_DIR ?? '').trim();
    if (override !== '') return override;
    return platform === 'darwin' ? join(home, 'Library', 'Logs', 'Big Smooth') : join(home, '.smooth', 'logs');
}

export interface RotatingLogOptions {
    /** Rotate once the file exceeds this many bytes. */
    maxBytes?: number;
    /** How many rotated generations (`.1` … `.N`) to keep. */
    keep?: number;
    /** Injected clock for tests. */
    now?: () => Date;
}

/** Append-only log with size rotation: `file` → `file.1` → … → `file.keep`.
 * Every write is synchronous (a crash line must land before we do anything
 * else) and never throws — logging must not take the app down. */
export class RotatingLog {
    private fd: number | undefined;
    private size = 0;
    private readonly maxBytes: number;
    private readonly keep: number;
    private readonly now: () => Date;

    constructor(
        readonly path: string,
        opts: RotatingLogOptions = {},
    ) {
        this.maxBytes = opts.maxBytes ?? 5 * 1024 * 1024;
        this.keep = opts.keep ?? 3;
        this.now = opts.now ?? (() => new Date());
    }

    /** Append one timestamped line. */
    line(text: string): void {
        this.raw(`${this.now().toISOString()} ${text}\n`);
    }

    /** Append raw bytes (the child's stdout/stderr, already newline-terminated). */
    raw(chunk: string | Uint8Array): void {
        try {
            const buf = typeof chunk === 'string' ? Buffer.from(chunk, 'utf8') : Buffer.from(chunk);
            if (buf.length === 0) return;
            if (this.fd === undefined) this.open();
            if (this.fd === undefined) return;
            if (this.size + buf.length > this.maxBytes && this.size > 0) {
                this.rotate();
                if (this.fd === undefined) return;
            }
            writeSync(this.fd, buf);
            this.size += buf.length;
        } catch {
            // Best effort.
        }
    }

    close(): void {
        if (this.fd !== undefined) {
            try {
                closeSync(this.fd);
            } catch {
                // already closed
            }
            this.fd = undefined;
        }
    }

    private open(): void {
        mkdirSync(dirname(this.path), { recursive: true });
        this.fd = openSync(this.path, 'a');
        this.size = existsSync(this.path) ? statSync(this.path).size : 0;
    }

    private rotate(): void {
        this.close();
        rotateFiles(this.path, this.keep);
        this.open();
    }
}

/** `path.N-1` → `path.N`, …, `path` → `path.1`; drops what falls past `keep`. */
export function rotateFiles(path: string, keep: number): void {
    const gen = (n: number) => `${path}.${n}`;
    if (keep <= 0) {
        if (existsSync(path)) unlinkSync(path);
        return;
    }
    if (existsSync(gen(keep))) unlinkSync(gen(keep));
    for (let n = keep - 1; n >= 1; n--) {
        if (existsSync(gen(n))) renameSync(gen(n), gen(n + 1));
    }
    if (existsSync(path)) renameSync(path, gen(1));
}

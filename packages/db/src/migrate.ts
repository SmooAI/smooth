/** Auto-create SQLite tables on startup using Drizzle push */

import Database from 'better-sqlite3';
import { existsSync, mkdirSync } from 'node:fs';
import { homedir } from 'node:os';
import { dirname, join } from 'node:path';

import { memories } from './schema/memory.js';
import { workerRuns } from './schema/workers.js';
import { config } from './schema/config.js';

const defaultDbPath = join(homedir(), '.smooth', 'smooth.db');

/**
 * Ensure all tables exist in the SQLite database.
 * Uses raw SQL CREATE TABLE IF NOT EXISTS for reliability.
 * Called once on leader startup.
 */
export function ensureSchema(dbPath?: string): void {
    const path = dbPath ?? process.env.SMOOTH_DB_PATH ?? defaultDbPath;
    const dir = dirname(path);
    if (!existsSync(dir)) {
        mkdirSync(dir, { recursive: true });
    }

    const sqlite = new Database(path);
    sqlite.pragma('journal_mode = WAL');

    sqlite.exec(`
        CREATE TABLE IF NOT EXISTS memories (
            id TEXT PRIMARY KEY,
            content TEXT NOT NULL,
            metadata TEXT DEFAULT '{}',
            created_at INTEGER DEFAULT (unixepoch()) NOT NULL,
            updated_at INTEGER DEFAULT (unixepoch()) NOT NULL
        );

        CREATE TABLE IF NOT EXISTS worker_runs (
            id TEXT PRIMARY KEY,
            bead_id TEXT NOT NULL,
            worker_id TEXT NOT NULL,
            sandbox_id TEXT,
            backend_type TEXT NOT NULL DEFAULT 'local-microsandbox',
            phase TEXT NOT NULL,
            status TEXT NOT NULL,
            started_at INTEGER DEFAULT (unixepoch()) NOT NULL,
            completed_at INTEGER,
            metadata TEXT DEFAULT '{}'
        );

        CREATE TABLE IF NOT EXISTS config (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL,
            updated_at INTEGER DEFAULT (unixepoch()) NOT NULL
        );

        CREATE INDEX IF NOT EXISTS idx_worker_runs_bead ON worker_runs(bead_id);
        CREATE INDEX IF NOT EXISTS idx_worker_runs_status ON worker_runs(status);
        CREATE INDEX IF NOT EXISTS idx_memories_created ON memories(created_at);
    `);

    sqlite.close();
}

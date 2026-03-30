import Database from 'better-sqlite3';
import { drizzle } from 'drizzle-orm/better-sqlite3';
import { existsSync, mkdirSync } from 'node:fs';
import { homedir } from 'node:os';
import { dirname, join } from 'node:path';

import * as schema from './schema/index.js';

const defaultDbPath = join(homedir(), '.smooth', 'smooth.db');
const dbPath = process.env.SMOOTH_DB_PATH ?? defaultDbPath;

// Ensure directory exists
const dir = dirname(dbPath);
if (!existsSync(dir)) {
    mkdirSync(dir, { recursive: true });
}

const sqlite = new Database(dbPath);

// Enable WAL mode for better concurrent read performance
sqlite.pragma('journal_mode = WAL');
sqlite.pragma('foreign_keys = ON');

export const db = drizzle(sqlite, { schema });

export type SmoothDatabase = typeof db;

/** Get the database file path */
export function getDbPath(): string {
    return dbPath;
}

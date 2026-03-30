/** ~/.smooth/config.json and credentials.json management */

import { existsSync, mkdirSync, readFileSync, writeFileSync, chmodSync } from 'node:fs';
import { homedir } from 'node:os';
import { join } from 'node:path';

const SMOOTH_DIR = join(homedir(), '.smooth');
const CONFIG_PATH = join(SMOOTH_DIR, 'config.json');
const CREDENTIALS_PATH = join(SMOOTH_DIR, 'credentials.json');

export interface SmoothCliConfig {
    servers: Record<string, string>;
    active_server: string;
    jira?: {
        url: string;
        project: string;
        email: string;
    };
    smoo?: {
        api_url: string;
        org_id: string;
        client_id: string;
    };
    tailscale?: {
        tailnet: string;
    };
}

export interface Credentials {
    [serverUrl: string]: { api_key: string };
}

function ensureDir(): void {
    if (!existsSync(SMOOTH_DIR)) {
        mkdirSync(SMOOTH_DIR, { recursive: true });
    }
}

// ── Config ──────────────────────────────────────────────────

export function loadConfig(): SmoothCliConfig {
    ensureDir();
    if (!existsSync(CONFIG_PATH)) {
        return {
            servers: { default: 'http://localhost:4400' },
            active_server: 'default',
        };
    }
    return JSON.parse(readFileSync(CONFIG_PATH, 'utf8'));
}

export function saveConfig(config: SmoothCliConfig): void {
    ensureDir();
    writeFileSync(CONFIG_PATH, JSON.stringify(config, null, 4) + '\n', 'utf8');
}

export function getActiveServerUrl(): string {
    const config = loadConfig();
    return config.servers[config.active_server] ?? 'http://localhost:4400';
}

export function setConfigValue(key: string, value: string): void {
    const config = loadConfig();
    const parts = key.split('.');
    const obj = config as unknown as Record<string, unknown>;

    if (parts.length === 1) {
        obj[key] = value;
    } else {
        const [section, field] = parts;
        if (!obj[section]) {
            obj[section] = {};
        }
        (obj[section] as Record<string, string>)[field] = value;
    }

    saveConfig(config);
}

// ── Credentials ─────────────────────────────────────────────

export function loadCredentials(): Credentials {
    ensureDir();
    if (!existsSync(CREDENTIALS_PATH)) {
        return {};
    }
    return JSON.parse(readFileSync(CREDENTIALS_PATH, 'utf8'));
}

export function saveCredentials(creds: Credentials): void {
    ensureDir();
    writeFileSync(CREDENTIALS_PATH, JSON.stringify(creds, null, 4) + '\n', { mode: 0o600 });
    chmodSync(CREDENTIALS_PATH, 0o600);
}

export function getApiKey(serverUrl?: string): string | null {
    const url = serverUrl ?? getActiveServerUrl();
    const creds = loadCredentials();
    return creds[url]?.api_key ?? null;
}

export function setApiKey(serverUrl: string, apiKey: string): void {
    const creds = loadCredentials();
    creds[serverUrl] = { api_key: apiKey };
    saveCredentials(creds);
}

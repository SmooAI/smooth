//! Tiny persisted settings for the desktop shell (currently just the chosen
//! daemon target). Stored as JSON under Electron's per-app userData dir so it
//! survives restarts and app updates.

import { existsSync, readFileSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';

import { app } from 'electron';

export interface DesktopConfig {
    /** A full remote daemon URL to attach to (tailnet), or null for the local one. */
    remoteUrl: string | null;
}

const DEFAULTS: DesktopConfig = { remoteUrl: null };

function configPath(): string {
    return join(app.getPath('userData'), 'config.json');
}

export function loadConfig(): DesktopConfig {
    const path = configPath();
    if (!existsSync(path)) return { ...DEFAULTS };
    try {
        return { ...DEFAULTS, ...(JSON.parse(readFileSync(path, 'utf8')) as Partial<DesktopConfig>) };
    } catch {
        // A corrupt config shouldn't brick the app — fall back to local.
        return { ...DEFAULTS };
    }
}

export function saveConfig(config: DesktopConfig): void {
    writeFileSync(configPath(), `${JSON.stringify(config, null, 2)}\n`);
}

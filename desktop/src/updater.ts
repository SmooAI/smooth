//! Over-the-air updates via electron-updater.
//!
//! electron-builder bakes the `publish` block from electron-builder.yml into the
//! app's `app-update.yml`; `autoUpdater` reads it to find `latest-mac.yml` (and
//! the delta `.zip`) on the CDN. macOS only accepts an update whose new app is
//! signed with the SAME Developer ID and notarized — Squirrel.Mac refuses
//! anything else — which is exactly why the signing/notarization work had to land
//! first.
//!
//! Feed lives at https://downloads.smoo.ai/bigsmooth/ (S3 + CloudFront).

import { appendFileSync, mkdirSync } from 'node:fs';
import { homedir } from 'node:os';
import { dirname, join } from 'node:path';

import { app, dialog } from 'electron';
import electronUpdater from 'electron-updater';

import { stopDaemon } from './daemon.js';

const { autoUpdater } = electronUpdater;

// Check at launch, then every 30 min. A beta moves fast; the old 6h meant a new
// build could sit unseen for most of a day (th-updater-fix). Unpackaged/dev runs
// have no feed, so we skip entirely.
const CHECK_INTERVAL_MS = 30 * 60 * 1000;

/** Where update activity is logged. Same file the daemon spawn diagnostics use,
 * so "why didn't it update?" is answerable — the console is lost under `open`. */
const LOG_PATH = join(homedir(), '.smooth', 'desktop.log');

function logLine(level: string, ...args: unknown[]): void {
    const parts = args.map((a) => (a instanceof Error ? (a.stack ?? a.message) : typeof a === 'string' ? a : JSON.stringify(a)));
    const line = `${new Date().toISOString()} [updater:${level}] ${parts.join(' ')}\n`;
    try {
        mkdirSync(dirname(LOG_PATH), { recursive: true });
        appendFileSync(LOG_PATH, line);
    } catch {
        // best-effort; never let logging break the updater
    }
    // Also to console for a Terminal-launched run.
    console.log(line.trimEnd());
}

/** electron-updater's logger interface → our file logger, so every check,
 * download, and error is visible in ~/.smooth/desktop.log. */
const fileLogger = {
    info: (...a: unknown[]) => logLine('info', ...a),
    warn: (...a: unknown[]) => logLine('warn', ...a),
    error: (...a: unknown[]) => logLine('error', ...a),
    debug: (...a: unknown[]) => logLine('debug', ...a),
};

/** Start silent background update checks. Safe no-op in a dev/unpackaged run. */
export function startAutoUpdates(): void {
    if (!app.isPackaged) return;
    autoUpdater.logger = fileLogger;
    autoUpdater.autoDownload = true;
    // The differential (delta) downloader assembles the new zip from the old one
    // + a blockmap, then verifies the result's sha512 against latest-mac.yml. Our
    // publish pipeline produces a blockmap that doesn't reassemble byte-exact
    // (notarization/stapling shifts bytes after the blockmap is written), so it
    // always fails the checksum and falls back to a full download anyway. Skip the
    // doomed partial attempt and full-download directly — same result, no wasted
    // bandwidth or scary error in the log. (th-updater-fix)
    autoUpdater.disableDifferentialDownload = true;
    autoUpdater.on('checking-for-update', () => logLine('info', 'checking for update…'));
    autoUpdater.on('update-available', (info) => logLine('info', `update available: ${info.version}`));
    autoUpdater.on('update-not-available', (info) => logLine('info', `up to date (${info.version})`));
    autoUpdater.on('update-downloaded', (info) => {
        logLine('info', `update downloaded: ${info.version}`);
        void dialog
            .showMessageBox({
                type: 'info',
                buttons: ['Restart now', 'Later'],
                defaultId: 0,
                cancelId: 1,
                message: `Big Smooth ${info.version} is ready.`,
                detail: 'Restart to finish updating. Your session is preserved.',
            })
            .then(async ({ response }) => {
                if (response !== 0) return;
                // th-79416c: fully stop the daemon and WAIT for it to exit BEFORE
                // handing the bundle to Squirrel. A fire-and-forget SIGTERM raced
                // the installer — the daemon was still holding files inside
                // Big Smooth.app when the ditto/copy ran, so the swap failed
                // intermittently and rolled back. Awaiting frees the bundle first.
                logLine('info', 'stopping daemon before install…');
                await stopDaemon();
                logLine('info', 'daemon stopped; quitAndInstall');
                autoUpdater.quitAndInstall();
            });
    });
    autoUpdater.on('error', (err) => logLine('error', err));
    void autoUpdater.checkForUpdates();
    setInterval(() => void autoUpdater.checkForUpdates(), CHECK_INTERVAL_MS);
}

/**
 * Manual "Check for Updates…" — unlike the silent background check, this reports
 * the already-up-to-date case so the menu item gives feedback. An available
 * update flows through the normal download → `update-downloaded` restart prompt.
 */
export async function checkForUpdatesInteractive(): Promise<void> {
    if (!app.isPackaged) {
        void dialog.showMessageBox({ type: 'info', message: 'Updates are only available in the installed app.' });
        return;
    }
    try {
        const result = await autoUpdater.checkForUpdates();
        if (!result || result.updateInfo.version === app.getVersion()) {
            void dialog.showMessageBox({ type: 'info', message: `You’re up to date (${app.getVersion()}).` });
        }
    } catch (err) {
        void dialog.showMessageBox({ type: 'warning', message: 'Could not check for updates.', detail: String(err) });
    }
}

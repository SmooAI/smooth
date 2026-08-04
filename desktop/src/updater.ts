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

import { app, dialog } from 'electron';
import electronUpdater from 'electron-updater';

const { autoUpdater } = electronUpdater;

// Check at launch, then every 6h. Unpackaged/dev runs have no feed, so skip.
const CHECK_INTERVAL_MS = 6 * 60 * 60 * 1000;

/** Start silent background update checks. Safe no-op in a dev/unpackaged run. */
export function startAutoUpdates(): void {
    if (!app.isPackaged) return;
    autoUpdater.autoDownload = true;
    autoUpdater.on('update-downloaded', (info) => {
        void dialog
            .showMessageBox({
                type: 'info',
                buttons: ['Restart now', 'Later'],
                defaultId: 0,
                cancelId: 1,
                message: `Big Smooth ${info.version} is ready.`,
                detail: 'Restart to finish updating. Your session is preserved.',
            })
            .then(({ response }) => {
                // before-quit stops the daemon; quitAndInstall relaunches into the new app.
                if (response === 0) autoUpdater.quitAndInstall();
            });
    });
    autoUpdater.on('error', (err) => console.error('[updater]', err));
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

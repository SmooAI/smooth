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
//!
//! Resilience (th-d4feb8): an update that Squirrel won't install (bad signature
//! / corrupt zip) used to re-offer + re-prompt every 30 min forever, and
//! `update-downloaded` could fire twice and race two `quitAndInstall`s. We now
//! (1) act at most once per version per session, (2) guard against a duplicate
//! install, and (3) after an update repeatedly fails to stick, stop nagging and
//! offer a manual download. See updateDecision.ts for the pure logic.
//!
//! Sparkle-style choice dialog (th-75eb2e): Electron can't use Sparkle — that's a
//! native/AppKit updater, and it's what the native macOS companion SmoothFlow
//! uses — but we reproduce its UX with Electron's own native
//! `dialog.showMessageBox`. When an update is AVAILABLE we present the same three
//! choices Sparkle offers (Skip This Version / Remind Me Later / Install Update)
//! plus its "Automatically download and install updates in the future" checkbox.
//! `autoDownload` is therefore OFF: nothing downloads until the user picks Install
//! (or has previously opted into auto). Install → `downloadUpdate()`, then the
//! existing `update-downloaded` restart step (with all its give-up/attempt-cap
//! resilience) takes over. Skip persists the version to a skip list; Remind Me
//! Later just defers to the next launch/interval; the checkbox persists the
//! auto-download preference.

import { appendFileSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { homedir } from 'node:os';
import { dirname, join } from 'node:path';

import { app, dialog, shell } from 'electron';
import electronUpdater from 'electron-updater';

import { loadConfig, saveConfig } from './config.js';
import { stopDaemon } from './daemon.js';
import { addSkippedVersion, decideAvailableAction, decideUpdateAction, recordAttempt, shouldClearState, type UpdateState } from './updateDecision.js';

const { autoUpdater } = electronUpdater;

// Check at launch, then every 30 min. A beta moves fast; the old 6h meant a new
// build could sit unseen for most of a day (th-updater-fix). Unpackaged/dev runs
// have no feed, so we skip entirely.
const CHECK_INTERVAL_MS = 30 * 60 * 1000;

/** Where update activity is logged. Same file the daemon spawn diagnostics use,
 * so "why didn't it update?" is answerable — the console is lost under `open`. */
const LOG_PATH = join(homedir(), '.smooth', 'desktop.log');

/** Cross-restart record of the version we're trying to install + how many times
 * we've tried, so a failed install that relaunches the OLD app is remembered. */
const STATE_PATH = join(homedir(), '.smooth', 'desktop-update-state.json');

/** Versions we've already shown the RESTART dialog for THIS session — so a
 * re-emitted `update-downloaded` (electron-updater fires it on every check while a
 * download is pending) doesn't re-prompt. */
const promptedThisSession = new Set<string>();

/** Versions we've already shown the AVAILABLE (Sparkle-style choice) dialog for
 * THIS session — so the 30-min background poll doesn't re-nag after a "Remind Me
 * Later". Separate from `promptedThisSession` because the available dialog and the
 * restart dialog are two distinct prompts for the same version. */
const availablePromptedThisSession = new Set<string>();

/** True from the moment the user opts to restart (or a download starts) until the
 * process exits, so a duplicate event can't start a second stopDaemon→quitAndInstall
 * (the 3s double-fire that raced Squirrel and rolled back — th-d4feb8). */
let installing = false;

/** Set for the duration of an explicit "Check for Updates…" so the shared
 * `update-available` handler knows to always show the dialog (the user asked),
 * bypassing the skip list / auto-download / once-per-session guards that only
 * suppress the background nag. */
let interactiveCheck = false;

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

function readState(): UpdateState | null {
    try {
        const raw = JSON.parse(readFileSync(STATE_PATH, 'utf8')) as Partial<UpdateState>;
        if (typeof raw.version === 'string' && typeof raw.attempts === 'number') return { version: raw.version, attempts: raw.attempts };
    } catch {
        // no state yet / unreadable — treat as none
    }
    return null;
}

function writeState(state: UpdateState | null): void {
    try {
        mkdirSync(dirname(STATE_PATH), { recursive: true });
        writeFileSync(STATE_PATH, state ? JSON.stringify(state) : '{}');
    } catch {
        // best-effort; a lost counter just means one extra restart attempt
    }
}

/** The versioned DMG on the CDN for THIS arch — the manual-download fallback when
 * auto-install won't stick. Matches electron-builder's `${productName}-${version}-${arch}.dmg`
 * (confirmed against latest-mac.yml, e.g. `Big Smooth-0.1.12-arm64.dmg`). */
function dmgUrl(version: string): string {
    const arch = process.arch === 'x64' ? 'x64' : 'arm64';
    return `https://downloads.smoo.ai/bigsmooth/${encodeURIComponent(`Big Smooth-${version}-${arch}.dmg`)}`;
}

/** electron-updater's logger interface → our file logger, so every check,
 * download, and error is visible in ~/.smooth/desktop.log. */
const fileLogger = {
    info: (...a: unknown[]) => logLine('info', ...a),
    warn: (...a: unknown[]) => logLine('warn', ...a),
    error: (...a: unknown[]) => logLine('error', ...a),
    debug: (...a: unknown[]) => logLine('debug', ...a),
};

/** The "couldn't finish updating automatically — download it" fallback. Shown
 * once per session per version, so a permanently-uninstallable artifact gives a
 * working path instead of an endless restart nag. */
function offerManualDownload(version: string): void {
    void dialog
        .showMessageBox({
            type: 'warning',
            buttons: ['Download update', 'Later'],
            defaultId: 0,
            cancelId: 1,
            message: `Big Smooth ${version} couldn’t finish updating automatically.`,
            detail: 'Download the latest version and drag it into your Applications folder to replace this copy.',
        })
        .then(({ response }) => {
            if (response === 0) void shell.openExternal(dmgUrl(version));
        });
}

/** The Sparkle-style "A new version is available" choice dialog, reproduced with
 * Electron's native message box. Mirrors SmoothFlow's Sparkle dialog: title, the
 * "X is now available—you have Y" body, the auto-update checkbox, and the three
 * Skip / Later / Install buttons. Resolves after the user's choice is applied. */
async function showAvailableDialog(version: string): Promise<void> {
    const cfg = loadConfig();
    // Buttons, indexed. macOS renders them right-to-left with the default last,
    // matching Sparkle's Skip · Later · Install order.
    const SKIP = 0;
    const LATER = 1;
    const INSTALL = 2;
    const { response, checkboxChecked } = await dialog.showMessageBox({
        type: 'info',
        buttons: ['Skip This Version', 'Remind Me Later', 'Install Update'],
        defaultId: INSTALL,
        cancelId: LATER,
        message: 'A new version of Big Smooth is available!',
        detail: `Big Smooth ${version} is now available—you have ${app.getVersion()}. Would you like to download it now?`,
        checkboxLabel: 'Automatically download and install updates in the future',
        checkboxChecked: cfg.autoUpdate,
    });

    // The checkbox is remembered regardless of which button was pressed — Sparkle
    // treats it as a standing preference for FUTURE versions, so a Skip here still
    // arms auto-download for the next release.
    if (checkboxChecked !== cfg.autoUpdate) {
        saveConfig({ autoUpdate: checkboxChecked });
        logLine('info', `auto-update preference → ${checkboxChecked}`);
    }

    if (response === SKIP) {
        saveConfig({ skippedUpdateVersions: addSkippedVersion(cfg.skippedUpdateVersions, version) });
        logLine('info', `user skipped ${version} — will not be offered again`);
        return;
    }
    if (response === LATER) {
        logLine('info', `user deferred ${version} — will re-offer next launch`);
        return;
    }
    // Install Update → begin the download; `update-downloaded` handles the restart.
    logLine('info', `user chose to install ${version} — downloading…`);
    void autoUpdater.downloadUpdate();
}

/** Handle an `update-available` event: decide, then either show the Sparkle-style
 * dialog, silently download (auto-update opted in), or ignore. Factored out so the
 * decision + wiring reads top-to-bottom. */
function onUpdateAvailable(info: UpdateInfoLike): void {
    const cfg = loadConfig();
    const persisted = readState();
    const wasInteractive = interactiveCheck;
    interactiveCheck = false; // consume the one-shot flag

    const action = decideAvailableAction({
        availableVersion: info.version,
        installedVersion: app.getVersion(),
        skippedVersions: cfg.skippedUpdateVersions,
        autoUpdate: cfg.autoUpdate,
        promptedThisSession: availablePromptedThisSession.has(info.version),
        installing,
        persisted,
        interactive: wasInteractive,
    });
    logLine('info', `update available: ${info.version} (installed ${app.getVersion()}) → ${action}${wasInteractive ? ' [interactive]' : ''}`);

    if (action === 'ignore') return;
    availablePromptedThisSession.add(info.version);

    if (action === 'auto-download') {
        logLine('info', `auto-update on — downloading ${info.version} silently`);
        void autoUpdater.downloadUpdate();
        return;
    }
    void showAvailableDialog(info.version);
}

/** The shape of electron-updater's `UpdateInfo` we actually read — kept local so
 * the pure decision layer stays free of the dependency's types. */
interface UpdateInfoLike {
    version: string;
}

/** Minimal numeric semver compare (major.minor.patch), enough to tell whether the
 * running version has reached the one we were installing. Non-numeric/extra parts
 * are ignored; returns <0, 0, or >0. */
function cmpSemver(a: string, b: string): number {
    const pa = a.split('.').map((n) => Number.parseInt(n, 10) || 0);
    const pb = b.split('.').map((n) => Number.parseInt(n, 10) || 0);
    for (let i = 0; i < 3; i++) {
        const d = (pa[i] ?? 0) - (pb[i] ?? 0);
        if (d !== 0) return d;
    }
    return 0;
}

/** Start silent background update checks. Safe no-op in a dev/unpackaged run. */
export function startAutoUpdates(): void {
    if (!app.isPackaged) return;
    autoUpdater.logger = fileLogger;
    // Off, on purpose: we present the Sparkle-style choice dialog on
    // `update-available` and only start the download when the user picks Install
    // (or has opted into auto-download). With autoDownload on, electron-updater
    // would fetch the bundle before we could ask. (th-75eb2e)
    autoUpdater.autoDownload = false;
    // The differential (delta) downloader assembles the new zip from the old one
    // + a blockmap, then verifies the result's sha512 against latest-mac.yml. Our
    // publish pipeline produces a blockmap that doesn't reassemble byte-exact
    // (notarization/stapling shifts bytes after the blockmap is written), so it
    // always fails the checksum and falls back to a full download anyway. Skip the
    // doomed partial attempt and full-download directly — same result, no wasted
    // bandwidth or scary error in the log. (th-updater-fix)
    autoUpdater.disableDifferentialDownload = true;

    // The update we were trying to install finally stuck (the running version has
    // reached it) → clear the attempt counter so a FUTURE update starts fresh.
    const startupState = readState();
    if (shouldClearState(startupState, app.getVersion(), (a, b) => cmpSemver(a, b) >= 0)) {
        logLine('info', `now on ${app.getVersion()} — clearing stale update state (was targeting ${startupState?.version})`);
        writeState(null);
    }

    autoUpdater.on('checking-for-update', () => logLine('info', 'checking for update…'));
    autoUpdater.on('update-available', (info) => onUpdateAvailable(info));
    autoUpdater.on('update-not-available', (info) => logLine('info', `up to date (${info.version})`));
    autoUpdater.on('update-downloaded', (info) => {
        const persisted = readState();
        const action = decideUpdateAction({
            downloadedVersion: info.version,
            installedVersion: app.getVersion(),
            promptedThisSession: promptedThisSession.has(info.version),
            installing,
            persisted,
        });
        logLine('info', `update downloaded: ${info.version} (installed ${app.getVersion()}, attempts ${persisted?.attempts ?? 0}) → ${action}`);
        if (action === 'ignore') return;

        // Whatever we do next, we've now "spoken" for this version this session.
        promptedThisSession.add(info.version);

        if (action === 'give-up') {
            logLine('warn', `update ${info.version} has failed to install ${persisted?.attempts ?? 0}× — offering manual download`);
            offerManualDownload(info.version);
            return;
        }

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
                if (installing) return; // a concurrent dialog already started the install
                installing = true;
                // Record the attempt BEFORE installing, so a failed swap that
                // relaunches the old app still counts against MAX_INSTALL_ATTEMPTS.
                writeState(recordAttempt(persisted, info.version));
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
 * the already-up-to-date case so the menu item gives feedback, and it always shows
 * the Sparkle-style choice dialog for an available update even if that version was
 * skipped or already offered this session (the user explicitly asked). The
 * `interactiveCheck` flag tells the shared `update-available` handler to force the
 * prompt; the available update then flows Install → download → restart as usual.
 */
export async function checkForUpdatesInteractive(): Promise<void> {
    if (!app.isPackaged) {
        void dialog.showMessageBox({ type: 'info', message: 'Updates are only available in the installed app.' });
        return;
    }
    try {
        interactiveCheck = true;
        const result = await autoUpdater.checkForUpdates();
        if (!result || result.updateInfo.version === app.getVersion()) {
            interactiveCheck = false; // no available update fired; nothing consumed the flag
            void dialog.showMessageBox({ type: 'info', message: `You’re up to date (${app.getVersion()}).` });
        }
    } catch (err) {
        interactiveCheck = false;
        void dialog.showMessageBox({ type: 'warning', message: 'Could not check for updates.', detail: String(err) });
    }
}

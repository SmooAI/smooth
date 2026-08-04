//! Big Smooth desktop — the Electron shell around the native `smooth-daemon`.
//!
//! The daemon serves the smooth-web SPA at `/` with its local auth token already
//! injected into `index.html`, so the window is just a BrowserWindow pointed at
//! `http://127.0.0.1:<port>/` — exactly what the browser PWA loads. No preload,
//! no IPC, no renderer code of our own.

import { spawn } from 'node:child_process';
import { join } from 'node:path';

import { app, BrowserWindow, dialog, Menu, nativeImage, shell, Tray } from 'electron';

import { baseUrl, runDaemonCommand, startDaemon, stopDaemon } from './daemon.js';
import { checkForUpdatesInteractive, startAutoUpdates } from './updater.js';

let win: BrowserWindow | undefined;
let tray: Tray | undefined;
let quitting = false;
let spawnedDaemon = false;

if (!app.requestSingleInstanceLock()) {
    app.quit();
} else {
    app.on('second-instance', showWindow);
    app.whenReady().then(main);
}

async function main(): Promise<void> {
    // Packaged builds get the th mark from BigSmooth.icns via electron-builder;
    // an unpackaged run would otherwise show the stock Electron icon.
    if (process.platform === 'darwin' && !app.isPackaged) app.dock?.setIcon(asset('icon.png'));
    createTray();
    const result = await startDaemon();
    spawnedDaemon = result.spawned;
    if (!result.ok) {
        dialog.showErrorBox('Big Smooth could not start', result.error ?? 'The daemon failed to start.');
        app.quit();
        return;
    }
    showWindow();
    startAutoUpdates();
}

function showWindow(): void {
    if (win) {
        if (win.isMinimized()) win.restore();
        win.show();
        win.focus();
        return;
    }
    win = new BrowserWindow({
        width: 1100,
        height: 800,
        minWidth: 480,
        minHeight: 480,
        title: 'Big Smooth',
        // ponytail: a normal title bar. `hiddenInset` floats the traffic lights
        // over the page, straight on top of smooth-web's top-left sidebar toggle.
        // Going frameless again means insetting the SPA's header, which is
        // smooth-web's business and would move the browser PWA too.
        backgroundColor: '#0b0f14',
        icon: asset('icon.png'),
        webPreferences: { nodeIntegration: false, contextIsolation: true },
    });
    win.loadURL(`${baseUrl()}/`);
    // Closing the window parks the app in the tray; only Quit exits.
    win.on('close', (e) => {
        if (quitting) return;
        e.preventDefault();
        win?.hide();
    });
    win.on('closed', () => {
        win = undefined;
    });
    // Anything not on the daemon's origin (docs, OAuth consent, …) belongs in the browser.
    win.webContents.setWindowOpenHandler(({ url }) => {
        shell.openExternal(url);
        return { action: 'deny' };
    });
}

function createTray(): void {
    const icon = nativeImage.createFromPath(asset('tray.png'));
    tray = new Tray(icon);
    tray.setToolTip('Big Smooth');
    tray.setContextMenu(
        Menu.buildFromTemplate([
            { label: 'Open Big Smooth', click: showWindow },
            { label: 'Check for Updates…', click: () => void checkForUpdatesInteractive() },
            { type: 'separator' },
            {
                label: 'Set Up',
                // Every entry is a macOS TCC gate; nothing to set up elsewhere.
                visible: process.platform === 'darwin',
                submenu: [
                    { label: 'Calendar…', click: () => grantEventKit('calendar') },
                    { label: 'Reminders…', click: () => grantEventKit('reminders') },
                    { label: 'Messages…', click: grantAppleEvents },
                    { label: 'Full Disk Access…', click: openFullDiskAccess },
                ],
            },
            { type: 'separator' },
            { label: 'Quit Big Smooth', click: () => app.quit() },
        ]),
    );
    tray.on('click', showWindow);
}

/**
 * Calendar / Reminders: `smooth-daemon tcc <what>` as our child, so TCC names
 * THIS bundle in the prompt and reads its usage strings (see runDaemonCommand).
 */
async function grantEventKit(what: 'calendar' | 'reminders'): Promise<void> {
    const { ok, output } = await runDaemonCommand(['tcc', what]);
    // The daemon prints `<what>: granted|denied|not-determined`; a denial can only
    // be undone in System Settings, so say so rather than silently doing nothing.
    dialog.showMessageBox({
        type: ok && output.includes('granted') ? 'info' : 'warning',
        message: output || 'No response from the daemon.',
        detail: output.includes('denied') ? 'Grant it in System Settings › Privacy & Security.' : undefined,
    });
}

/**
 * Messages: the gate is Automation (Apple Events), and it fires the first time
 * something actually talks to Messages.app. Poking it with `osascript` as our
 * child is the whole flow — same attribution rule, no daemon involvement.
 */
function grantAppleEvents(): void {
    spawn('/usr/bin/osascript', ['-e', 'tell application "Messages" to count of windows']);
}

/**
 * Full Disk Access has no prompt — it can only be granted by hand. Open the pane
 * and reveal the app so it can be dragged in.
 */
function openFullDiskAccess(): void {
    shell.openExternal('x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles');
    // Reveal the .app itself, not the executable buried inside it — the pane
    // wants the bundle dragged in.
    shell.showItemInFolder(app.getPath('exe').replace(/(\.app)\/Contents\/MacOS\/.*$/, '$1'));
}

function asset(name: string): string {
    return join(import.meta.dirname, '..', 'assets', name);
}

app.on('before-quit', () => {
    quitting = true;
    if (spawnedDaemon) stopDaemon();
});

// Staying resident with no windows is the point — the tray is the app.
app.on('window-all-closed', () => {});
app.on('activate', showWindow);

//! Big Smooth desktop — the Electron shell around the native `smooth-daemon`.
//!
//! The daemon serves the smooth-web SPA at `/` with its local auth token already
//! injected into `index.html`, so the window is just a BrowserWindow pointed at
//! `http://127.0.0.1:<port>/` — exactly what the browser PWA loads. No preload,
//! no IPC, no renderer code of our own.

import { spawn } from 'node:child_process';
import { join } from 'node:path';

import { app, BrowserWindow, dialog, Menu, nativeImage, shell, Tray } from 'electron';

import { baseUrl, startDaemon, stopDaemon } from './daemon.js';

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
    createTray();
    const result = await startDaemon();
    spawnedDaemon = result.spawned;
    if (!result.ok) {
        dialog.showErrorBox('Big Smooth could not start', result.error ?? 'The daemon failed to start.');
        app.quit();
        return;
    }
    showWindow();
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
        titleBarStyle: process.platform === 'darwin' ? 'hiddenInset' : 'default',
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
            { type: 'separator' },
            {
                label: 'Set Up',
                submenu: [
                    { label: 'Calendar…', click: () => runSetup('--setup-calendar') },
                    { label: 'Reminders…', click: () => runSetup('--setup-reminders') },
                    { label: 'Messages…', click: () => runSetup('--setup-imessage') },
                    { label: 'Full Disk Access…', click: () => runSetup('--fix-fda') },
                ],
            },
            { type: 'separator' },
            { label: 'Quit Big Smooth', click: () => app.quit() },
        ]),
    );
    tray.on('click', showWindow);
}

/**
 * The setup flows are interactive `th doctor` commands (they prompt, and the OS
 * permission prompts must come from a real GUI session), so hand them a terminal
 * rather than reimplementing them here.
 *
 * ponytail: shells out to Terminal.app on macOS and shows the command elsewhere —
 * swap in a real in-app flow when the daemon exposes these over HTTP.
 */
function runSetup(flag: string): void {
    const command = `th doctor ${flag}`;
    if (process.platform === 'darwin') {
        spawn('/usr/bin/osascript', ['-e', `tell application "Terminal" to do script "${command}"`, '-e', 'tell application "Terminal" to activate']);
        return;
    }
    dialog.showMessageBox({ type: 'info', message: 'Run this in a terminal:', detail: command });
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

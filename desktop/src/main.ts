//! Big Smooth desktop — the Electron shell around the native `smooth-daemon`.
//!
//! The daemon serves the smooth-web SPA at `/` with its local auth token already
//! injected into `index.html`, so the window is just a BrowserWindow pointed at
//! `http://127.0.0.1:<port>/` — exactly what the browser PWA loads. No preload,
//! no IPC, no renderer code of our own.

import { spawn } from 'node:child_process';
import { join } from 'node:path';

import { app, BrowserWindow, dialog, ipcMain, Menu, nativeImage, shell, Tray } from 'electron';

import { loadConfig, saveConfig } from './config.js';
import { baseUrl, isRemote, remoteUrl, resolveThBin, runDaemonCommand, startDaemon, stopDaemon } from './daemon.js';
import { discoverDaemons, type RemoteDaemon } from './discovery.js';
import { linkThOnPath } from './installth.js';
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
    // Pick the daemon target BEFORE anything reads it: the saved remote URL (a
    // tailnet daemon like smoo-hub) or the local one. daemon.ts reads this env.
    const cfg = loadConfig();
    if (cfg.remoteUrl) process.env.SMOOTH_REMOTE_URL = cfg.remoteUrl;

    // Packaged builds get the th mark from BigSmooth.icns via electron-builder;
    // an unpackaged run would otherwise show the stock Electron icon.
    if (process.platform === 'darwin' && !app.isPackaged) app.dock?.setIcon(asset('icon.png'));
    registerIpc();
    createTray();
    installThCli();
    const result = await startDaemon();
    spawnedDaemon = result.spawned;
    if (!result.ok) {
        // A dead LOCAL daemon is fatal; a dead REMOTE one shouldn't strand the app —
        // offer to fall back to This Mac (the tray stays alive either way to reconnect).
        if (isRemote()) {
            const { response } = await dialog.showMessageBox({
                type: 'warning',
                buttons: ['Switch to This Mac', 'Quit'],
                defaultId: 0,
                cancelId: 1,
                message: 'Can’t reach the remote Big Smooth',
                detail: result.error ?? '',
            });
            if (response === 0) connectTo(null);
            else app.quit();
            return;
        }
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
        title: windowTitle(),
        // ponytail: a normal title bar. `hiddenInset` floats the traffic lights
        // over the page, straight on top of smooth-web's top-left sidebar toggle.
        // Going frameless again means insetting the SPA's header, which is
        // smooth-web's business and would move the browser PWA too.
        backgroundColor: '#0b0f14',
        icon: asset('icon.png'),
        // sandbox off so the ESM preload can load (Electron 40); the surface it
        // exposes is a two-method contextBridge, nothing else.
        webPreferences: { nodeIntegration: false, contextIsolation: true, sandbox: false, preload: join(import.meta.dirname, 'preload.js') },
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

/** The window/label name for the current target: "Big Smooth" locally, or
 * "Big Smooth — <host>" when attached to a remote (tailnet) daemon. */
function windowTitle(): string {
    if (!isRemote()) return 'Big Smooth';
    try {
        return `Big Smooth — ${new URL(baseUrl()).hostname.split('.')[0]}`;
    } catch {
        return 'Big Smooth (remote)';
    }
}

function createTray(): void {
    const icon = nativeImage.createFromPath(asset('tray.png'));
    // Template image: macOS renders the black silhouette in the menu-bar colour —
    // white on a dark bar, black on light — so the `th` mark stays legible instead
    // of a low-contrast teal. No-op off macOS.
    icon.setTemplateImage(true);
    tray = new Tray(icon);
    tray.setToolTip('Big Smooth');
    tray.on('click', showWindow);
    applyTrayMenu([]); // first paint with no discovered peers yet…
    void refreshDiscovery(); // …then discover tailnet daemons and repaint.
}

/** (Re)build the tray menu, including the Connect submenu from `daemons`. */
function applyTrayMenu(daemons: RemoteDaemon[]): void {
    const active = remoteUrl(); // '' = local
    const connect: Electron.MenuItemConstructorOptions[] = [{ label: 'This Mac (local)', type: 'radio', checked: active === '', click: () => connectTo(null) }];
    if (daemons.length > 0) {
        connect.push({ type: 'separator' });
        for (const d of daemons) {
            connect.push({
                label: d.identity ? `${d.name} — ${d.identity}` : d.name,
                type: 'radio',
                checked: active === d.url,
                click: () => connectTo(d.url),
            });
        }
    }
    connect.push({ type: 'separator' }, { label: 'Refresh', click: () => void refreshDiscovery() });

    tray?.setContextMenu(
        Menu.buildFromTemplate([
            { label: 'Open Big Smooth', click: showWindow },
            { label: 'Check for Updates…', click: () => void checkForUpdatesInteractive() },
            { type: 'separator' },
            { label: 'Connect', submenu: connect },
            {
                label: 'Set Up',
                // TCC gates are on THIS Mac's local daemon; irrelevant when attached
                // to a remote one, so hide it there.
                visible: process.platform === 'darwin' && !isRemote(),
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
}

/** Discover tailnet daemons and repaint the tray's Connect submenu. */
async function refreshDiscovery(): Promise<void> {
    try {
        applyTrayMenu(await discoverDaemons());
    } catch {
        applyTrayMenu([]);
    }
}

/** IPC the preload exposes to the SPA's Settings → Connection: the same
 * list + switch the tray Connect menu drives, so either surface can reconnect. */
function registerIpc(): void {
    ipcMain.handle('daemons:list', async () => ({ current: remoteUrl() || null, daemons: await discoverDaemons() }));
    ipcMain.handle('daemons:connect', (_event, url: unknown) => {
        connectTo(typeof url === 'string' && url !== '' ? url : null);
    });
}

/** Switch the daemon target (null = local) and relaunch so `main()` re-reads it —
 * `before-quit` stops any local daemon we spawned, then we reconnect. */
function connectTo(url: string | null): void {
    if ((loadConfig().remoteUrl ?? null) === (url ?? null)) {
        showWindow();
        return;
    }
    saveConfig({ remoteUrl: url });
    app.relaunch();
    app.quit();
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

/**
 * Symlink the bundled `th` onto PATH so it works from a terminal, and so OTA
 * updates (which replace the whole app bundle) carry `th` too. Best-effort: never
 * let a PATH-link failure block the app. See installth.ts for the safety rule
 * (a real brew/curl `th` is left alone, never clobbered).
 */
function installThCli(): void {
    try {
        const res = linkThOnPath(resolveThBin());
        if (res.action === 'skipped-regular-file') {
            console.log(`th: leaving your own \`th\` at ${res.path} in place (not from Big Smooth).`);
        } else if (res.action === 'created' || res.action === 'repointed') {
            console.log(`th: ${res.action} ${res.path} → bundled CLI.`);
        } else if (res.action === 'no-writable-dir') {
            console.log('th: no writable PATH dir; run the CLI from the app bundle or add ~/.local/bin to PATH.');
        }
    } catch (err) {
        console.log(`th: could not link onto PATH: ${String(err)}`);
    }
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

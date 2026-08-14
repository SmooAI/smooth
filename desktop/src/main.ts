//! Big Smooth desktop — the Electron shell around the native `smooth-daemon`.
//!
//! The daemon serves the smooth-web SPA at `/` with its local auth token already
//! injected into `index.html`, so the window is just a BrowserWindow pointed at
//! `http://127.0.0.1:<port>/` — exactly what the browser PWA loads. No preload,
//! no IPC, no renderer code of our own.

import { spawn } from 'node:child_process';
import { join } from 'node:path';

import { app, BrowserWindow, dialog, ipcMain, Menu, nativeImage, Notification, shell, Tray } from 'electron';

import { isNewChatChord } from './newchat.js';
import { buildNotification, type NotifyPayload } from './notify.js';

import { type DesktopConfig, loadConfig, saveConfig } from './config.js';
import {
    baseUrl,
    desktopLog,
    isHealthy,
    isRemote,
    remoteUrl,
    resolveThBin,
    runDaemonCommand,
    startDaemon,
    stopDaemon,
    tccHelperApp,
    tccOpenArgs,
} from './daemon.js';
import { discoverDaemons, type RemoteDaemon } from './discovery.js';
import { linkThOnPath } from './installth.js';
import { firstRunLoginItem, trayModeLabel } from './loginitem.js';
import { checkForUpdatesInteractive, startAutoUpdates } from './updater.js';

let win: BrowserWindow | undefined;
let tray: Tray | undefined;
let quitting = false;
let spawnedDaemon = false;
let lastDaemons: RemoteDaemon[] = [];

if (!app.requestSingleInstanceLock()) {
    app.quit();
} else {
    app.on('second-instance', showWindow);
    app.whenReady().then(main);
}

async function main(): Promise<void> {
    // The remote URL (a tailnet daemon like smoo-hub) is only the WINDOW's view
    // target — it must be read before baseUrl(), but it no longer gates whether we
    // run our own daemon. daemon.ts reads this env.
    const cfg = loadConfig();
    if (cfg.remoteUrl) process.env.SMOOTH_REMOTE_URL = cfg.remoteUrl;

    // Packaged builds get the th mark from BigSmooth.icns via electron-builder;
    // an unpackaged run would otherwise show the stock Electron icon.
    if (process.platform === 'darwin' && !app.isPackaged) app.dock?.setIcon(asset('icon.png'));
    registerIpc();
    applyLoginItemDefault(cfg); // th-ccf2cf: open-at-login on first run
    createTray();
    installThCli();

    // th-5c2ec6: ALWAYS start this Mac's own daemon. A stale saved remoteUrl used
    // to early-return here and leave the machine with no local daemon (phone
    // offline). Remote is a view target, not a reason to skip the local agent.
    const result = await startDaemon();
    spawnedDaemon = result.spawned;
    if (!result.ok) desktopLog(`local daemon not running: ${result.error ?? 'unknown error'}`);

    if (isRemote()) {
        // The window points at a remote daemon; the local one runs in the
        // background (best-effort — a thin client may not even have the binary).
        if (!(await isHealthy(baseUrl()))) {
            const { response } = await dialog.showMessageBox({
                type: 'warning',
                buttons: ['Switch to This Mac', 'Quit'],
                defaultId: 0,
                cancelId: 1,
                message: 'Can’t reach the remote Big Smooth',
                detail: `${baseUrl()} isn’t reachable — is it running and are you on the tailnet?`,
            });
            if (response === 0) connectTo(null);
            else app.quit();
            return;
        }
    } else if (!result.ok) {
        // Local mode: the window has nothing to load without the local daemon.
        dialog.showErrorBox('Big Smooth could not start', result.error ?? 'The daemon failed to start.');
        app.quit();
        return;
    }
    showWindow();
    startAutoUpdates();
}

/**
 * First-run only: turn on "open at login" so the daemon comes back after a reboot,
 * then never touch it again — the tray checkbox (and System Settings) own it from
 * there. Persisted via `loginItemConfigured` so it's applied exactly once.
 */
function applyLoginItemDefault(cfg: DesktopConfig): void {
    const decision = firstRunLoginItem(cfg.loginItemConfigured);
    if (!decision) return;
    try {
        app.setLoginItemSettings({ openAtLogin: decision.setOpenAtLogin });
    } catch (err) {
        desktopLog(`login item: could not set openAtLogin: ${String(err)}`);
    }
    saveConfig({ loginItemConfigured: true });
}

/** Flip "open at login" from the tray, then repaint so the checkbox reflects it. */
function toggleOpenAtLogin(): void {
    try {
        const current = app.getLoginItemSettings().openAtLogin;
        app.setLoginItemSettings({ openAtLogin: !current });
    } catch (err) {
        desktopLog(`login item: could not toggle openAtLogin: ${String(err)}`);
    }
    applyTrayMenu(lastDaemons);
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
    // Cmd+N (macOS) / Ctrl+N (Windows/Linux) → new chat (th-6ab65a). Handled at
    // the window level rather than via an application Menu: adding a Menu would
    // replace Electron's default one and we'd have to re-declare the standard
    // Edit roles or lose copy/paste. `before-input-event` claims the chord before
    // the page sees it and forwards it to the SPA, which starts a fresh
    // conversation (the same action as the sidebar's "New chat").
    win.webContents.on('before-input-event', (event, input) => {
        if (isNewChatChord(input)) {
            event.preventDefault();
            win?.webContents.send('new-chat');
        }
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

/** Short hostname of a daemon URL for display (`https://smoo-hub…:8443` → `smoo-hub`). */
function hostOf(url: string): string {
    try {
        return new URL(url).hostname.split('.')[0];
    } catch {
        return 'remote';
    }
}

/** `openAtLogin` state, never throwing (setLoginItemSettings is unsupported on
 * some platforms/dev runs); defaults to false so the checkbox stays coherent. */
function safeOpenAtLogin(): boolean {
    try {
        return app.getLoginItemSettings().openAtLogin;
    } catch {
        return false;
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
    lastDaemons = daemons; // so toggles (e.g. Open at Login) can repaint without re-discovering
    const active = remoteUrl(); // '' = local
    const openAtLogin = safeOpenAtLogin();
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
            // Current mode, unmissable at a glance — remote mode used to be visible
            // only in the (nested) Connect submenu and the title bar (th-5c2ec6).
            { label: trayModeLabel(active === '' ? null : hostOf(active)), enabled: false },
            { type: 'separator' },
            { label: 'Open Big Smooth', click: showWindow },
            { label: 'Open at Login', type: 'checkbox', checked: openAtLogin, click: toggleOpenAtLogin },
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
    // Native notifications (th-6af4b1): the Electron webview can't receive Web
    // Push, so the SPA's `usePush` posts here and we show a real OS notification.
    // Clicking it surfaces the window. Returns whether it was shown.
    ipcMain.handle('notify:show', (_event, payload: unknown) => {
        if (!Notification.isSupported()) return false;
        const n = buildNotification((payload ?? {}) as NotifyPayload);
        if (!n) return false;
        const notif = new Notification({ title: n.title, body: n.body });
        notif.on('click', showWindow);
        notif.show();
        return true;
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
 * Calendar / Reminders. macOS shows the EventKit prompt only for a signed
 * bundle whose MAIN executable asks — the daemon spawned as our child can't (it
 * returns not-determined silently). So we launch the nested helper bundle
 * (Contents/Helpers/BigSmoothTCC.app, main exe = smooth-daemon) via `open`,
 * which prompts, then poll the grant status (`open` can't return the child's
 * stdout, and reading status as a child works even though asking doesn't).
 */
async function grantEventKit(what: 'calendar' | 'reminders'): Promise<void> {
    const helper = tccHelperApp();
    if (!helper) {
        // Unpackaged/dev build: no helper bundle. Fall back to the child path,
        // which at least reports current status (it just can't prompt).
        const { output } = await runDaemonCommand(['tcc', what]);
        dialog.showMessageBox({ type: 'warning', message: output || 'TCC helper is only available in the packaged app.' });
        return;
    }
    // Fire the prompt (LaunchServices attributes it to the helper bundle).
    spawn('/usr/bin/open', tccOpenArgs(helper, what), { stdio: 'ignore' });
    const status = await pollTccStatus(what);
    dialog.showMessageBox({
        type: status === 'granted' ? 'info' : 'warning',
        message: `${what}: ${status}`,
        detail:
            status === 'denied'
                ? 'Grant it in System Settings › Privacy & Security.'
                : status === 'not-determined'
                  ? 'No answer yet — try Set Up again, or check System Settings › Privacy & Security.'
                  : undefined,
    });
}

/** Poll the daemon's `tcc <what>` status (as our child — reading works, asking
 * doesn't) until it leaves `not-determined` or ~2 min elapses. That window
 * matches the daemon's own EventKit request cap; the user is answering a dialog. */
async function pollTccStatus(what: 'calendar' | 'reminders'): Promise<string> {
    const deadline = Date.now() + 120_000;
    let last = 'not-determined';
    while (Date.now() < deadline) {
        const { output } = await runDaemonCommand(['tcc', what]);
        const match = /granted|denied|not-determined/.exec(output);
        if (match) {
            last = match[0];
            if (last !== 'not-determined') return last;
        }
        await new Promise((r) => setTimeout(r, 1000));
    }
    return last;
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

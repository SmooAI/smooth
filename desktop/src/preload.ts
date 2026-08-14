//! Preload bridge: the ONLY thing the renderer (the smooth-web SPA) can reach in
//! the Electron main. Exposes a narrow, typed surface over contextBridge so the
//! web Settings page can list + switch daemon targets — the same actions the tray
//! Connect menu drives. Everything else stays walled off (contextIsolation).

import { contextBridge, ipcRenderer } from 'electron';

contextBridge.exposeInMainWorld('bigSmooth', {
    /** Current target + discovered tailnet daemons: `{ current: string|null, daemons: {name,url,identity?}[] }`. */
    listDaemons: (): Promise<unknown> => ipcRenderer.invoke('daemons:list'),
    /** Switch target and relaunch. `null` ⇒ This Mac (local). */
    connectTo: (url: string | null): Promise<void> => ipcRenderer.invoke('daemons:connect', url),
    /** Show a native OS notification (th-6af4b1). The Electron webview can't do
     * Web Push, so `usePush` calls this instead. Resolves to whether it showed. */
    notify: (payload: { title?: string; body?: string; deepLink?: string }): Promise<boolean> => ipcRenderer.invoke('notify:show', payload),
    /** Cmd/Ctrl+N from the window → start a new chat. The main process claims the
     * chord and forwards a `new-chat` IPC; the SPA runs its "New chat" action.
     * Returns an unsubscribe fn so the renderer can detach on unmount. */
    onNewChat: (cb: () => void): (() => void) => {
        const handler = (): void => cb();
        ipcRenderer.on('new-chat', handler);
        return () => ipcRenderer.removeListener('new-chat', handler);
    },
});

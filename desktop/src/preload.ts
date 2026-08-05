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
});

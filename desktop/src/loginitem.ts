//! Open-at-login decision logic, kept free of any `electron` import so it can be
//! unit-tested under plain `node --test`. The wiring (`app.setLoginItemSettings`)
//! lives in main.ts; this file only decides *what* to set.

/**
 * First-run default for the login item: turn "open at login" ON exactly once, then
 * step aside. Returns the `openAtLogin` value to apply, or `null` when it's already
 * been configured (the user's own toggle owns it from then on).
 *
 * Idempotent by the `configured` flag: a stale/leftover config that never ran this
 * still gets the default; a machine that has run it keeps whatever the user chose.
 */
export function firstRunLoginItem(configured: boolean): { setOpenAtLogin: boolean } | null {
    return configured ? null : { setOpenAtLogin: true };
}

/** Human-readable one-liner for the tray's current-mode header. `remoteHost` is
 * the remote daemon's hostname when viewing a remote, or `null` for local. In
 * remote mode the local daemon still runs — the app always owns its own daemon —
 * so we say so, since that's exactly the state that used to be invisible. */
export function trayModeLabel(remoteHost: string | null): string {
    return remoteHost ? `Viewing ${remoteHost} · This Mac's daemon still running` : 'Running on This Mac';
}

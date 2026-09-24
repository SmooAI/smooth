//! Whether the app is really quitting (th-6b5d5c).
//!
//! Closing the window normally parks Big Smooth in the tray: the `close`
//! handler cancels the close and hides the window. That handler has to let the
//! close through whenever the app is actually exiting, or the exit never
//! happens.
//!
//! The flag used to live in `main.ts` and was only set in `before-quit`. That
//! works for Quit from the tray, but not for an update: `quitAndInstall()`
//! closes every window FIRST and only calls `app.quit()` once they have all
//! closed, so `before-quit` fires too late. The close was cancelled, the app
//! never exited, and Squirrel's ShipIt gave up with "App Still Running Error",
//! so the update never installed. The updater now marks the quit here before
//! it calls `quitAndInstall()`.
//!
//! Electron-free so it can be unit-tested under plain Node.

let quitting = false;

/** Record that the app is exiting (a real Quit, or an update install). */
export function markQuitting(): void {
    quitting = true;
}

/** True once the app has started a real exit. */
export function isQuitting(): boolean {
    return quitting;
}

/** Whether a window `close` should be turned into "hide to tray". Never while
 * the app is exiting — that close is part of the exit. */
export function shouldHideOnClose(exiting: boolean = quitting): boolean {
    return !exiting;
}

/** Test-only: reset between cases. */
export function resetQuittingForTests(): void {
    quitting = false;
}

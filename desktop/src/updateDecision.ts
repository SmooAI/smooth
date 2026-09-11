//! Pure decision logic for the OTA updater — no electron imports, so it's
//! unit-testable under `node --test` (updater.ts itself pulls in `app`/`dialog`
//! and can't load in the test runner).
//!
//! Context (th-d4feb8): a published update that Squirrel refuses to install
//! (bad signature / corrupt zip) can never advance the version, so the old
//! updater re-offered it and re-prompted to restart every 30 minutes forever —
//! and `update-downloaded` could even fire twice, racing two `quitAndInstall`s.
//! These helpers make the updater fire once per version, and give up (with a
//! manual-download fallback) after an update repeatedly fails to stick.

/** Persisted across app restarts (~/.smooth/desktop-update-state.json) so a
 * failed install that relaunches the OLD version is still remembered next launch.
 * `attempts` counts how many times we've asked Squirrel to install `version`. */
export interface UpdateState {
    version: string;
    attempts: number;
}

/** How many times to let an update try to install before we stop auto-prompting
 * for it and offer a manual download instead. Two clean attempts, then fall back. */
export const MAX_INSTALL_ATTEMPTS = 2;

export type UpdateAction =
    /** Prompt to restart & install (the normal path). */
    | 'prompt-install'
    /** Do nothing — already installing, already prompted this session, or the
     * "new" version isn't actually newer than what's running. */
    | 'ignore'
    /** This version has failed to install too many times — stop nagging to
     * restart; offer a one-time manual download instead. */
    | 'give-up';

/** Decide what to do when electron-updater reports a downloaded update. Pure. */
export function decideUpdateAction(opts: {
    downloadedVersion: string;
    installedVersion: string;
    /** Have we already shown a dialog for this exact version this session? */
    promptedThisSession: boolean;
    /** Are we already mid-install (guards a duplicate `update-downloaded`)? */
    installing: boolean;
    /** Persisted cross-restart attempt record, or null if none. */
    persisted: UpdateState | null;
    maxAttempts?: number;
}): UpdateAction {
    const { downloadedVersion, installedVersion, promptedThisSession, installing, persisted } = opts;
    const maxAttempts = opts.maxAttempts ?? MAX_INSTALL_ATTEMPTS;

    // A swap already in flight, or we already spoke this session — never stack a
    // second dialog / second quitAndInstall (the 3s double-fire bug).
    if (installing) return 'ignore';
    if (promptedThisSession) return 'ignore';
    // Defensive: electron-updater shouldn't offer the running version, but if it
    // does (stale pending in cache), don't act on it.
    if (downloadedVersion === installedVersion) return 'ignore';

    // This exact version has already burned through its install attempts and the
    // app is STILL not on it → the artifact won't install here. Stop the restart
    // nag; the caller offers a manual download instead.
    if (persisted && persisted.version === downloadedVersion && persisted.attempts >= maxAttempts) {
        return 'give-up';
    }
    return 'prompt-install';
}

/** What to do when electron-updater reports an update is AVAILABLE (before it's
 * downloaded). This drives the Sparkle-style choice dialog. Kept separate from
 * `decideUpdateAction` (which drives the post-download restart step) so both
 * layers stay pure and independently testable. */
export type UpdateAvailableAction =
    /** Show the native "A new version is available" dialog (Skip / Later / Install). */
    | 'prompt'
    /** The user previously opted into automatic updates — download silently, no dialog. */
    | 'auto-download'
    /** Do nothing — already installing, already offered this session, the version was
     * skipped, it isn't newer, or it has already burned through its install attempts. */
    | 'ignore';

/** Decide what to do when electron-updater reports an available (not-yet-downloaded)
 * update. Pure — no electron, no I/O. The caller supplies the persisted preferences
 * (skip list, auto-update flag) and session/attempt state. */
export function decideAvailableAction(opts: {
    availableVersion: string;
    installedVersion: string;
    /** Versions the user explicitly chose "Skip This Version" for (persisted). */
    skippedVersions: readonly string[];
    /** Persisted "Automatically download and install updates in the future" checkbox. */
    autoUpdate: boolean;
    /** Have we already shown the available-dialog for this exact version this session? */
    promptedThisSession: boolean;
    /** Are we already mid-install (or mid-download after a choice)? */
    installing: boolean;
    /** Persisted cross-restart install-attempt record, or null. */
    persisted: UpdateState | null;
    /** Force the prompt regardless of skip/auto/session guards (an explicit
     * "Check for Updates…" — but still respect `installing`). */
    interactive?: boolean;
    maxAttempts?: number;
}): UpdateAvailableAction {
    const { availableVersion, installedVersion, skippedVersions, autoUpdate, promptedThisSession, installing, persisted, interactive } = opts;
    const maxAttempts = opts.maxAttempts ?? MAX_INSTALL_ATTEMPTS;

    // A download/install already in flight — never stack a second choice.
    if (installing) return 'ignore';

    // An explicit user-initiated check always shows the dialog (they asked), as
    // long as it's genuinely a newer version, ignoring the skip list / auto flag /
    // session guard that only suppress the *background* nag.
    if (interactive) {
        if (availableVersion === installedVersion) return 'ignore';
        return 'prompt';
    }

    if (promptedThisSession) return 'ignore';
    if (availableVersion === installedVersion) return 'ignore';
    if (skippedVersions.includes(availableVersion)) return 'ignore';

    // This exact version has already failed to install too many times — don't keep
    // (auto-)downloading a bundle that won't stick. The download path's own give-up
    // offers a manual download instead.
    if (persisted && persisted.version === availableVersion && persisted.attempts >= maxAttempts) {
        return 'ignore';
    }

    return autoUpdate ? 'auto-download' : 'prompt';
}

/** Append a version to the "skipped" list, de-duplicated. Pure helper so the
 * persistence call in updater.ts stays a one-liner. */
export function addSkippedVersion(skipped: readonly string[], version: string): string[] {
    return skipped.includes(version) ? [...skipped] : [...skipped, version];
}

/** The persisted record after we start an install attempt for `version`:
 * increments the counter when it's the same version, resets to 1 for a new one. */
export function recordAttempt(persisted: UpdateState | null, version: string): UpdateState {
    if (persisted && persisted.version === version) {
        return { version, attempts: persisted.attempts + 1 };
    }
    return { version, attempts: 1 };
}

/** Whether the persisted attempt record should be cleared given the version now
 * running — true once the app has reached (or passed) the version we were trying
 * to install, i.e. the update finally stuck. Compared with a caller-supplied
 * semver-gte so this module stays dependency-free. */
export function shouldClearState(persisted: UpdateState | null, installedVersion: string, gte: (a: string, b: string) => boolean): boolean {
    if (!persisted) return false;
    return gte(installedVersion, persisted.version);
}

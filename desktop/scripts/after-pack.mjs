// Assemble the nested TCC helper app inside Big Smooth.app.
//
// Why this exists: macOS shows the EventKit (Calendar/Reminders) permission
// prompt ONLY for a properly-signed bundle whose *main executable* asks. The
// Electron app's main executable is Electron, and it spawns `smooth-daemon` as
// a CHILD — a spawned child can inherit grants but is not allowed to *ask*, so
// `smooth-daemon tcc calendar` run from the app returns not-determined with no
// prompt (measured; see desktop/README.md "TCC").
//
// The fix: ship a tiny helper bundle whose CFBundleExecutable IS smooth-daemon,
// so it can be launched via `open` as a bundle main executable and thus prompt.
// grantEventKit() / `th doctor --setup-calendar` launch it with `tcc <what>`.
//
// electron-builder runs this AFTER packaging and BEFORE signing, so the helper
// is in place when @electron/osx-sign walks the bundle. osx-sign signs nested
// code recursively (it already signs the bundled smooth-daemon/th under
// Contents/Resources), so it signs this nested .app too — with the app's
// Developer ID + hardened runtime, which is what makes notarization + the TCC
// prompt work. No signing is done here: that would double-sign, and the
// dist:mac unsigned path leaves CSC_NAME set with no importable cert, which a
// hook-side codesign would choke on.

import { execFileSync } from 'node:child_process';
import { chmodSync, copyFileSync, existsSync, mkdirSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';

// The stable TCC key. Same identifier the native app bundle uses
// (scripts/macos/Info.plist), so the grant aligns with `smooth-daemon`'s own
// identity — the daemon and this helper are the same signed binary.
const BUNDLE_ID = 'ai.smoo.smooth-daemon';

// Usage strings copied from electron-builder.yml mac.extendInfo — macOS reads
// these off the ASKING bundle, so the helper needs its own copy.
const CAL_DESC = 'Big Smooth reads and edits your calendar when you ask it to.';
const REM_DESC = 'Big Smooth reads and edits your reminders when you ask it to.';

/** @param {import('electron-builder').AfterPackContext} context */
export default async function afterPack(context) {
    if (context.electronPlatformName !== 'darwin') return;

    const appName = `${context.packager.appInfo.productFilename}.app`;
    const appPath = join(context.appOutDir, appName);
    // extraResources stages the daemon at Contents/Resources/smooth-daemon.
    const daemonBin = join(appPath, 'Contents', 'Resources', 'smooth-daemon');
    if (!existsSync(daemonBin)) {
        console.warn(`after-pack: no smooth-daemon at ${daemonBin}; skipping TCC helper (stage-daemon must run first).`);
        return;
    }

    const helper = join(appPath, 'Contents', 'Helpers', 'BigSmoothTCC.app');
    const helperMacOS = join(helper, 'Contents', 'MacOS');
    mkdirSync(helperMacOS, { recursive: true });

    // ponytail: copy the binary rather than symlink — a CFBundleExecutable that
    // symlinks out of its own bundle confuses codesign and TCC.
    const helperBin = join(helperMacOS, 'smooth-daemon');
    copyFileSync(daemonBin, helperBin);
    chmodSync(helperBin, 0o755);

    const plistPath = join(helper, 'Contents', 'Info.plist');
    writeFileSync(plistPath, helperPlist());
    execFileSync('/usr/bin/plutil', ['-lint', plistPath], { stdio: 'ignore' });

    console.log(`after-pack: assembled TCC helper at ${helper} (electron-builder will sign it).`);
}

function helperPlist() {
    return `<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key>
    <string>Big Smooth TCC</string>
    <key>CFBundleIdentifier</key>
    <string>${BUNDLE_ID}</string>
    <key>CFBundleExecutable</key>
    <string>smooth-daemon</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleShortVersionString</key>
    <string>1.0</string>
    <key>CFBundleVersion</key>
    <string>1.0</string>
    <!-- NO LSUIElement: a background/agent app (LSUIElement=true) is not allowed
         to present the EventKit TCC prompt — macOS returns not-determined with no
         prompt. VERIFIED on macOS 26.4 (th-36da65): the identical helper WITHOUT
         LSUIElement prompts correctly when launched via `open`. This helper is a
         one-shot that exits as soon as the grant is answered, so the momentary
         foreground/Dock presence is harmless. -->
    <key>LSMinimumSystemVersion</key>
    <string>14.0</string>
    <key>NSCalendarsFullAccessUsageDescription</key>
    <string>${CAL_DESC}</string>
    <key>NSRemindersFullAccessUsageDescription</key>
    <string>${REM_DESC}</string>
</dict>
</plist>
`;
}

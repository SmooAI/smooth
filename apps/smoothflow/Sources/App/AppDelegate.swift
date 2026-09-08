import AppKit
import Sparkle

@MainActor
final class AppDelegate: NSObject, NSApplicationDelegate {
    let app = AppController()
    /// Sparkle OTA. Feed + key live in Info.plist (SUFeedURL / SUPublicEDKey);
    /// checks hourly on its own, the menu item is the manual path.
    let updater = SPUStandardUpdaterController(startingUpdater: true, updaterDelegate: nil, userDriverDelegate: nil)
    private var statusItem: NSStatusItem?

    func applicationDidFinishLaunching(_ notification: Notification) {
        NSApp.mainMenu = buildMenu()
        installStatusItem()
        app.start()
        NSApp.activate(ignoringOtherApps: true)
    }

    func applicationDidBecomeActive(_ notification: Notification) { app.activated() }
    func applicationWillTerminate(_ notification: Notification) { app.shutdown() }
    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool { false }

    func applicationShouldHandleReopen(_ sender: NSApplication, hasVisibleWindows flag: Bool) -> Bool {
        app.mainWindow.window?.makeKeyAndOrderFront(nil)
        return true
    }

    // MARK: menu bar

    private func buildMenu() -> NSMenu {
        let bar = NSMenu()

        let appMenu = NSMenu()
        appMenu.addItem(withTitle: "About SmoothFlow", action: #selector(NSApplication.orderFrontStandardAboutPanel(_:)), keyEquivalent: "")
        appMenu.addItem(checkForUpdatesItem())
        appMenu.addItem(.separator())
        appMenu.addItem(item("Settings…", #selector(showSettings), ","))
        let perms = NSMenu()
        for kind in PermissionKind.allCases {
            let it = NSMenuItem(title: kind.title + "…", action: #selector(grantPermission(_:)), keyEquivalent: "")
            it.representedObject = kind.rawValue
            it.target = self
            perms.addItem(it)
        }
        appMenu.addItem(submenu(perms, "Permissions"))
        appMenu.addItem(.separator())
        appMenu.addItem(withTitle: "Hide SmoothFlow", action: #selector(NSApplication.hide(_:)), keyEquivalent: "h")
        appMenu.addItem(withTitle: "Quit SmoothFlow", action: #selector(NSApplication.terminate(_:)), keyEquivalent: "q")
        bar.addItem(submenu(appMenu, "SmoothFlow"))

        let session = NSMenu()
        session.addItem(item("New Session…", #selector(newSession), "n"))
        session.addItem(item("Fan Out…", #selector(fanOut), "n", [.command, .shift]))
        session.addItem(.separator())
        session.addItem(item("Steer All Working", #selector(steerAll), "\r", [.command, .shift]))
        session.addItem(item("Steer Focused…", #selector(steerFocused), "\r", [.command]))
        session.addItem(.separator())
        session.addItem(item("Approve (Allow)", #selector(allow), "y", [.command, .option]))
        session.addItem(item("Deny", #selector(deny), "n", [.command, .option]))
        session.addItem(item("Kill & Resume", #selector(killResume), "r", [.command, .option]))
        session.addItem(item("Kill", #selector(kill), "k", [.command, .option]))
        session.addItem(.separator())
        for i in 1...9 {
            let it = item("Focus Session \(i)", #selector(focusIndex(_:)), String(i))
            it.tag = i - 1
            session.addItem(it)
        }
        bar.addItem(submenu(session, "Session"))

        let edit = NSMenu()
        edit.addItem(withTitle: "Cut", action: #selector(NSText.cut(_:)), keyEquivalent: "x")
        edit.addItem(withTitle: "Copy", action: #selector(NSText.copy(_:)), keyEquivalent: "c")
        edit.addItem(withTitle: "Paste", action: #selector(NSText.paste(_:)), keyEquivalent: "v")
        edit.addItem(withTitle: "Select All", action: #selector(NSText.selectAll(_:)), keyEquivalent: "a")
        bar.addItem(submenu(edit, "Edit"))

        let view = NSMenu()
        view.addItem(item("Inbox", #selector(inbox), "i"))
        view.addItem(.separator())
        view.addItem(item("Terminal", #selector(tabTerminal), "1", [.command, .option]))
        view.addItem(item("Diff", #selector(tabDiff), "2", [.command, .option]))
        view.addItem(item("PR", #selector(tabPR), "3", [.command, .option]))
        view.addItem(item("Activity", #selector(tabActivity), "4", [.command, .option]))
        view.addItem(.separator())
        view.addItem(item("Split Surface", #selector(split), "d"))
        view.addItem(item("Close Split", #selector(closeSplit), "w", [.command, .shift]))
        view.addItem(.separator())
        view.addItem(item("Toggle Sidebar", #selector(toggleSidebar), "s", [.command, .control]))
        view.addItem(item("Toggle Pearl Rail", #selector(toggleRail), "p", [.command, .control]))
        bar.addItem(submenu(view, "View"))

        let window = NSMenu()
        window.addItem(withTitle: "Minimize", action: #selector(NSWindow.miniaturize(_:)), keyEquivalent: "m")
        window.addItem(withTitle: "Zoom", action: #selector(NSWindow.zoom(_:)), keyEquivalent: "")
        bar.addItem(submenu(window, "Window"))
        NSApp.windowsMenu = window
        return bar
    }

    private func checkForUpdatesItem() -> NSMenuItem {
        let it = NSMenuItem(title: "Check for Updates…", action: #selector(SPUStandardUpdaterController.checkForUpdates(_:)), keyEquivalent: "")
        it.target = updater
        return it
    }

    // MARK: status item

    /// The window can be closed while the fleet keeps running
    /// (applicationShouldTerminateAfterLastWindowClosed = false); the menu-bar
    /// glyph is how you get back. Template image, so it follows the bar's
    /// appearance — Presence spends no color on chrome.
    private func installStatusItem() {
        let si = NSStatusBar.system.statusItem(withLength: NSStatusItem.squareLength)
        if let image = NSImage(named: "MenuBarTemplate") {
            image.isTemplate = true
            si.button?.image = image
        } else {
            si.button?.title = "th"
        }
        si.button?.toolTip = "SmoothFlow"
        let menu = NSMenu()
        menu.addItem(item("Open SmoothFlow", #selector(openMainWindow), ""))
        menu.addItem(item("Inbox", #selector(inbox), ""))
        menu.addItem(.separator())
        menu.addItem(checkForUpdatesItem())
        menu.addItem(item("Settings…", #selector(showSettings), ""))
        menu.addItem(.separator())
        menu.addItem(withTitle: "Quit SmoothFlow", action: #selector(NSApplication.terminate(_:)), keyEquivalent: "")
        si.menu = menu
        statusItem = si
    }

    @objc private func openMainWindow() {
        NSApp.activate(ignoringOtherApps: true)
        app.mainWindow.window?.makeKeyAndOrderFront(nil)
    }

    private func item(_ title: String, _ action: Selector, _ key: String, _ mods: NSEvent.ModifierFlags = [.command]) -> NSMenuItem {
        let it = NSMenuItem(title: title, action: action, keyEquivalent: key)
        it.keyEquivalentModifierMask = mods
        it.target = self
        return it
    }

    private func submenu(_ menu: NSMenu, _ title: String) -> NSMenuItem {
        let it = NSMenuItem(title: title, action: nil, keyEquivalent: "")
        it.submenu = menu
        menu.title = title
        return it
    }

    // MARK: actions

    @objc private func showSettings() { app.showSettings() }
    @objc private func grantPermission(_ sender: NSMenuItem) {
        if let raw = sender.representedObject as? String, let kind = PermissionKind(rawValue: raw) { app.permissions.request(kind) }
    }
    @objc private func newSession() { app.showNewSession() }
    @objc private func fanOut() { app.showFanOut() }
    @objc private func inbox() { app.toggleInbox() }
    @objc private func steerAll() { app.mainWindow.center.steerAll() }
    @objc private func steerFocused() { app.mainWindow.center.focusSteer() }
    @objc private func allow() { if let s = app.store.focused { app.approve(s, .allow) } }
    @objc private func deny() { if let s = app.store.focused { app.approve(s, .deny) } }
    @objc private func killResume() { if let s = app.store.focused { app.kill(s, resume: true) } }
    @objc private func kill() { if let s = app.store.focused { app.kill(s, resume: false) } }
    @objc private func focusIndex(_ sender: NSMenuItem) { app.focus(index: sender.tag) }
    @objc private func tabTerminal() { app.showTab(.terminal) }
    @objc private func tabDiff() { app.showTab(.diff) }
    @objc private func tabPR() { app.showTab(.pr) }
    @objc private func tabActivity() { app.showTab(.activity) }
    @objc private func split() { app.mainWindow.center.splitActive() }
    @objc private func closeSplit() { app.mainWindow.center.closeActivePane() }
    @objc private func toggleSidebar() { app.mainWindow.toggleSidebar() }
    @objc private func toggleRail() { app.mainWindow.toggleRail() }
}

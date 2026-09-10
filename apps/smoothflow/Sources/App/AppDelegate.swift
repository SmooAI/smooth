import AppKit
import Sparkle

@MainActor
final class AppDelegate: NSObject, NSApplicationDelegate {
    let app = AppController()
    /// Sparkle OTA. Feed + key live in Info.plist (SUFeedURL / SUPublicEDKey);
    /// checks hourly on its own, the menu item is the manual path.
    /// Not started under `SMOOTHFLOW_UI_TEST=1`: Sparkle's first-run "check automatically?" prompt would steal focus from a UI test.
    let updater = SPUStandardUpdaterController(startingUpdater: ProcessInfo.processInfo.environment["SMOOTHFLOW_UI_TEST"] != "1", updaterDelegate: nil, userDriverDelegate: nil)
    private var statusItem: NSStatusItem?

    func applicationDidFinishLaunching(_ notification: Notification) {
        // Before any surface or view asks for a font (th-bcd819).
        TerminalFont.registerBundled()
        app.keymap.onChange = { [weak self] in self?.rebuildMenu() }
        NSApp.mainMenu = buildMenu()
        // Hosting the unit tests: no status item, no window, no daemon, no tmux
        // (th-dccc80). Everything below `start()` is what a test would observe.
        guard AppController.shouldStart(env: ProcessInfo.processInfo.environment) else { return }
        installStatusItem()
        app.start()
        NSApp.activate(ignoringOtherApps: true)
    }

    func applicationDidBecomeActive(_ notification: Notification) { app.activated() }

    /// Every quit path — ⌘Q, the menu items, the status item, AppleScript
    /// `quit`, `NSRunningApplication.terminate()` (all of which arrive here
    /// through `NSApplication.terminate(_:)`) — takes the fleet down first,
    /// then terminates. Always `.terminateNow`: the shutdown is bounded on its
    /// own (th-6198bf), so there is never a `.terminateLater` to forget to
    /// answer, and never a `.terminateCancel` — quit means quit.
    func applicationShouldTerminate(_ sender: NSApplication) -> NSApplication.TerminateReply {
        app.shutdown()
        return .terminateNow
    }

    func applicationWillTerminate(_ notification: Notification) { app.shutdown() }
    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool { false }

    func applicationShouldHandleReopen(_ sender: NSApplication, hasVisibleWindows flag: Bool) -> Bool {
        app.mainWindow?.window?.makeKeyAndOrderFront(nil)
        return true
    }

    // MARK: menu bar

    /// Every shortcut in the bar comes from the keymap, so rebinding one in
    /// Settings ▸ Keyboard (or in the TOML file) moves the menu with it. The
    /// bar is rebuilt, not patched, whenever the map changes — cheap, and
    /// there is exactly one code path that can be wrong.
    private func buildMenu() -> NSMenu {
        let bar = NSMenu()

        let appMenu = NSMenu()
        appMenu.addItem(withTitle: "About SmoothFlow", action: #selector(NSApplication.orderFrontStandardAboutPanel(_:)), keyEquivalent: "")
        appMenu.addItem(checkForUpdatesItem())
        appMenu.addItem(.separator())
        appMenu.addItem(item(.settings))
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
        session.addItem(item(.newSession))
        session.addItem(item(.fanOut))
        session.addItem(.separator())
        session.addItem(item(.steerAll))
        session.addItem(item(.steerFocused))
        session.addItem(.separator())
        session.addItem(item(.allow))
        session.addItem(item(.deny))
        session.addItem(item(.killResume))
        session.addItem(item(.kill))
        session.addItem(.separator())
        for a in FlowAction.allCases where FlowAction.focusSessionIndex(a) != nil {
            let it = item(a)
            it.tag = FlowAction.focusSessionIndex(a) ?? 0
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
        view.addItem(item(.inbox))
        view.addItem(.separator())
        view.addItem(item(.viewTerminal))
        view.addItem(item(.viewDiff))
        view.addItem(item(.viewPR))
        view.addItem(item(.viewActivity))
        view.addItem(.separator())
        view.addItem(item(.toggleSidebar))
        view.addItem(item(.togglePearlRail))
        bar.addItem(submenu(view, "View"))

        // Tabs and splits are a menu of their own: they are the surface model,
        // not a view toggle, and burying them under View is how the old
        // single "Split Surface" item stayed unnoticed.
        let layout = NSMenu()
        layout.addItem(item(.newTab))
        layout.addItem(item(.newShell))
        layout.addItem(item(.closeTab))
        layout.addItem(.separator())
        layout.addItem(item(.previousTab))
        layout.addItem(item(.nextTab))
        layout.addItem(.separator())
        layout.addItem(item(.splitRight))
        layout.addItem(item(.splitDown))
        layout.addItem(item(.splitLeft))
        layout.addItem(item(.splitUp))
        layout.addItem(.separator())
        layout.addItem(item(.focusPaneLeft))
        layout.addItem(item(.focusPaneRight))
        layout.addItem(item(.focusPaneUp))
        layout.addItem(item(.focusPaneDown))
        layout.addItem(.separator())
        layout.addItem(item(.zoomPane))
        layout.addItem(item(.equalizePanes))
        layout.addItem(item(.closeSplit))
        bar.addItem(submenu(layout, "Layout"))

        let window = NSMenu()
        window.addItem(withTitle: "Minimize", action: #selector(NSWindow.miniaturize(_:)), keyEquivalent: "m")
        window.addItem(withTitle: "Zoom", action: #selector(NSWindow.zoom(_:)), keyEquivalent: "")
        bar.addItem(submenu(window, "Window"))
        NSApp.windowsMenu = window
        return bar
    }

    /// Rebuild the bar against the current keymap.
    func rebuildMenu() { NSApp.mainMenu = buildMenu() }

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
        menu.addItem(unbound(.inbox))
        menu.addItem(.separator())
        menu.addItem(checkForUpdatesItem())
        menu.addItem(unbound(.settings))
        menu.addItem(.separator())
        menu.addItem(withTitle: "Quit SmoothFlow", action: #selector(NSApplication.terminate(_:)), keyEquivalent: "")
        si.menu = menu
        statusItem = si
    }

    @objc private func openMainWindow() {
        NSApp.activate(ignoringOtherApps: true)
        app.mainWindow.window?.makeKeyAndOrderFront(nil)
    }

    /// A menu item for a `FlowAction`, wearing whatever chord the keymap
    /// currently gives it (none is legal — the item still works by mouse).
    private func item(_ action: FlowAction) -> NSMenuItem {
        let it = NSMenuItem(title: action.title, action: #selector(runAction(_:)), keyEquivalent: "")
        it.representedObject = action.rawValue
        if let chord = app.keymap.chord(for: action) {
            it.keyEquivalent = chord.menuKeyEquivalent
            it.keyEquivalentModifierMask = chord.menuModifiers
        }
        it.target = self
        return it
    }

    /// The same action with no key equivalent — the status-item menu, which
    /// must not claim a second copy of a shortcut the main bar already owns.
    private func unbound(_ action: FlowAction) -> NSMenuItem {
        let it = NSMenuItem(title: action.title, action: #selector(runAction(_:)), keyEquivalent: "")
        it.representedObject = action.rawValue
        it.target = self
        return it
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

    /// One selector for every keymap-driven item; the action rides on
    /// `representedObject`, so a rebind never has to touch a selector.
    @objc private func runAction(_ sender: NSMenuItem) {
        guard let raw = sender.representedObject as? String, let action = FlowAction(rawValue: raw) else { return }
        run(action, tag: sender.tag)
    }

    func run(_ action: FlowAction, tag: Int = 0) {
        let center = app.mainWindow?.center
        switch action {
        case .settings: app.showSettings()
        case .newSession: app.showNewSession()
        case .fanOut: app.showFanOut()
        case .steerAll: center?.steerAll()
        case .steerFocused: center?.focusSteer()
        case .allow: if let s = app.store.focused { app.approve(s, .allow) }
        case .deny: if let s = app.store.focused { app.approve(s, .deny) }
        case .killResume: if let s = app.store.focused { app.kill(s, resume: true) }
        case .kill: if let s = app.store.focused { app.kill(s, resume: false) }
        case .inbox: app.toggleInbox()
        case .viewTerminal: app.showTab(.terminal)
        case .viewDiff: app.showTab(.diff)
        case .viewPR: app.showTab(.pr)
        case .viewActivity: app.showTab(.activity)
        case .toggleSidebar: app.mainWindow?.toggleSidebar()
        case .togglePearlRail: app.mainWindow?.toggleRail()
        case .newTab: center?.newTab()
        case .newShell: center?.newShellHere()
        case .closeTab: center?.closeTab()
        case .previousTab: center?.cycleTab(by: -1)
        case .nextTab: center?.cycleTab(by: 1)
        case .splitRight: center?.split(.right)
        case .splitLeft: center?.split(.left)
        case .splitUp: center?.split(.up)
        case .splitDown: center?.split(.down)
        case .focusPaneLeft: center?.focusPane(.left)
        case .focusPaneRight: center?.focusPane(.right)
        case .focusPaneUp: center?.focusPane(.up)
        case .focusPaneDown: center?.focusPane(.down)
        case .zoomPane: center?.toggleZoom()
        case .equalizePanes: center?.equalizePanes()
        case .closeSplit: center?.closeActivePane()
        default: app.focus(index: FlowAction.focusSessionIndex(action) ?? tag)
        }
    }

    @objc private func grantPermission(_ sender: NSMenuItem) {
        if let raw = sender.representedObject as? String, let kind = PermissionKind(rawValue: raw) { app.permissions.request(kind) }
    }
}

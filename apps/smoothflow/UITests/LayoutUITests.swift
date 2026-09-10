import XCTest

/// th-27baa4: surface tabs, directional splits, and the Keyboard settings pane
/// against the mock fleet. These are the assertions that would catch a menu
/// item wired to the wrong action or a chord AppKit silently refuses — the
/// model itself is covered by KeymapTests / PaneTreeTests.
final class LayoutUITests: FlowUITestCase {
    override func setUpWithError() throws {
        try super.setUpWithError()
        try startMock()
        try launchApp()
    }

    private func selectWorkingSession() {
        waitForState("fs-1b8e05bb") { $0 == "working" }
        app.staticTexts["sidebar.title.fs-1b8e05bb"].click()
        XCTAssertTrue(waitUntil { self.label("pane.header").contains("th-1b8e05") }, label("pane.header"))
    }

    private var paneCount: Int {
        app.windows["SmoothFlow"].descendants(matching: .any).matching(NSPredicate(format: "identifier == %@", "pane.header")).count
    }

    func testCommandDSplitsRightAndCommandShiftWCloses() {
        selectWorkingSession()
        XCTAssertEqual(paneCount, 1)
        app.typeKey("d", modifierFlags: .command)
        XCTAssertTrue(waitUntil { self.paneCount == 2 }, "⌘D splits; tree: \(dump())")
        app.typeKey("d", modifierFlags: [.command, .shift])
        XCTAssertTrue(waitUntil { self.paneCount == 3 }, "⌘⇧D splits down")
        app.typeKey("w", modifierFlags: [.command, .shift])
        XCTAssertTrue(waitUntil { self.paneCount == 2 }, "⌘⇧W closes the focused split")
        app.typeKey("w", modifierFlags: [.command, .shift])
        XCTAssertTrue(waitUntil { self.paneCount == 1 })
        // The last pane is not closeable by ⌘⇧W — there would be nothing left.
        app.typeKey("w", modifierFlags: [.command, .shift])
        XCTAssertTrue(waitUntil { self.paneCount == 1 })
    }

    /// ⌘⇧↩ is Zoom Pane now (it used to be Steer All Working — see
    /// `FlowAction.defaultChord`). Zoomed, only one pane is on screen.
    func testCommandShiftReturnZoomsAndUnzooms() {
        selectWorkingSession()
        app.typeKey("d", modifierFlags: .command)
        XCTAssertTrue(waitUntil { self.paneCount == 2 })
        app.typeKey("\r", modifierFlags: [.command, .shift])
        XCTAssertTrue(waitUntil { self.paneCount == 1 }, "zoom fills the tab with one pane")
        app.typeKey("\r", modifierFlags: [.command, .shift])
        XCTAssertTrue(waitUntil { self.paneCount == 2 }, "unzoom restores the layout")
    }

    func testCommandTOpensATabAndCommandWClosesIt() {
        selectWorkingSession()
        let tabBar = app.windows["SmoothFlow"].descendants(matching: .any)["center.tabbar"]
        XCTAssertFalse(tabBar.exists, "one tab shows no tab bar")
        app.typeKey("t", modifierFlags: .command)
        XCTAssertTrue(tabBar.waitForExistence(timeout: 10), "⌘T opens a second tab; tree: \(dump())")
        XCTAssertTrue(app.windows["SmoothFlow"].descendants(matching: .any)["center.tab.1"].exists)
        app.typeKey("w", modifierFlags: .command)
        XCTAssertTrue(waitUntil { !tabBar.exists }, "⌘W closes it and the bar goes away")
    }

    /// A split lives in its tab: switching away and back must not lose it.
    func testSplitsBelongToTheirTab() {
        selectWorkingSession()
        app.typeKey("d", modifierFlags: .command)
        XCTAssertTrue(waitUntil { self.paneCount == 2 })
        app.typeKey("t", modifierFlags: .command)
        XCTAssertTrue(waitUntil { self.paneCount == 1 }, "the new tab starts with one pane")
        app.typeKey("[", modifierFlags: [.command, .shift])
        XCTAssertTrue(waitUntil { self.paneCount == 2 }, "⌘⇧[ goes back to the split tab")
        app.typeKey("]", modifierFlags: [.command, .shift])
        XCTAssertTrue(waitUntil { self.paneCount == 1 }, "⌘⇧] returns")
    }

    func testKeyboardSettingsPaneListsAndResetsBindings() {
        app.typeKey(",", modifierFlags: .command)
        let settings = app.windows["SmoothFlow Settings"]
        XCTAssertTrue(settings.waitForExistence(timeout: 10))
        settingsTab("Keyboard", in: settings).click()
        let pane = settings.descendants(matching: .any)["settings.pane.keyboard"]
        XCTAssertTrue(pane.waitForExistence(timeout: 10), "Keyboard pane; tree: \(dump())")
        let chord = settings.descendants(matching: .any)["settings.keyboard.chord.splitRight"]
        XCTAssertTrue(chord.waitForExistence(timeout: 10), "a row for Split Right")
        XCTAssertEqual(chord.value as? String ?? chord.label, "⌘D")
        // Reset-all is disabled until something is actually overridden.
        XCTAssertFalse(settings.buttons["settings.keyboard.resetAll"].isEnabled)
    }

    /// The menu is built from the keymap, so the Layout menu is where the new
    /// surface actions live and it must carry the shortcuts we shipped.
    func testLayoutMenuCarriesTheShippedShortcuts() {
        let layout = app.menuBars.menuBarItems["Layout"]
        XCTAssertTrue(layout.waitForExistence(timeout: 10), "Layout menu")
        layout.click()
        for title in ["New Tab", "Close Tab", "Split Right", "Split Down", "Split Left", "Split Up",
                      "Focus Pane Left", "Focus Pane Right", "Zoom Pane", "Equalize Panes", "Close Split"] {
            XCTAssertTrue(layout.menuItems[title].exists, "\(title) in the Layout menu")
        }
        layout.typeKey(XCUIKeyboardKey.escape, modifierFlags: [])
    }
}

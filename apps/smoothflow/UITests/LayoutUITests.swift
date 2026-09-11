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

    func testCommandDSplitsRightAndDown() {
        selectWorkingSession()
        XCTAssertEqual(paneCount, 1)
        app.typeKey("d", modifierFlags: .command)
        XCTAssertTrue(waitUntil { self.paneCount == 2 }, "⌘D splits; tree: \(dump())")
        app.typeKey("d", modifierFlags: [.command, .shift])
        XCTAssertTrue(waitUntil { self.paneCount == 3 }, "⌘⇧D splits down")
    }

    /// ⌘W on a pane holding a working agent must ask before it does anything,
    /// and Cancel must leave the pane exactly where it was.
    func testCommandWAsksBeforeClosingAPaneWithALiveAgent() {
        selectWorkingSession()
        app.typeKey("d", modifierFlags: .command)
        XCTAssertTrue(waitUntil { self.paneCount == 2 })
        app.typeKey("w", modifierFlags: .command)
        let cancel = app.windows["SmoothFlow"].descendants(matching: .any)["pane.close.cancel"]
        XCTAssertTrue(cancel.waitForExistence(timeout: 10), "the confirmation sheet; tree: \(dump())")
        XCTAssertTrue(app.descendants(matching: .any)["pane.close.kill"].exists, "End Session is offered")
        XCTAssertTrue(app.descendants(matching: .any)["pane.close.suppress"].exists, "Don't ask again")
        cancel.click()
        XCTAssertTrue(waitUntil { self.paneCount == 2 }, "Cancel leaves the pane alone")

        app.typeKey("w", modifierFlags: .command)
        let close = app.windows["SmoothFlow"].descendants(matching: .any)["pane.close.close"]
        XCTAssertTrue(close.waitForExistence(timeout: 10))
        close.click()
        XCTAssertTrue(waitUntil { self.paneCount == 1 }, "Close Pane closes it")
    }

    /// ⌘⇧W keeps its place: the whole tab, splits and all.
    func testCommandShiftWClosesTheWholeTab() {
        selectWorkingSession()
        app.typeKey("t", modifierFlags: .command)
        let tabBar = app.windows["SmoothFlow"].descendants(matching: .any)["center.tabbar"]
        XCTAssertTrue(tabBar.waitForExistence(timeout: 10))
        app.typeKey("d", modifierFlags: .command)
        XCTAssertTrue(waitUntil { self.paneCount == 2 }, "a split in the second tab")
        app.typeKey("w", modifierFlags: [.command, .shift])
        XCTAssertTrue(waitUntil { !tabBar.exists }, "⌘⇧W takes the tab and both its panes")
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

    /// ⌘T then ⌘W: the new tab's pane is empty, so ⌘W needs no confirmation
    /// and the tab collapses with it — the "container goes when it empties"
    /// half of the terminal semantics.
    func testCommandTOpensATabAndCommandWCollapsesItWhenEmpty() {
        let tabBar = app.windows["SmoothFlow"].descendants(matching: .any)["center.tabbar"]
        XCTAssertFalse(tabBar.exists, "one tab shows no tab bar")
        app.typeKey("t", modifierFlags: .command)
        XCTAssertTrue(tabBar.waitForExistence(timeout: 10), "⌘T opens a second tab; tree: \(dump())")
        XCTAssertTrue(app.windows["SmoothFlow"].descendants(matching: .any)["center.tab.1"].exists)
        app.typeKey("w", modifierFlags: .command)
        XCTAssertTrue(waitUntil { !tabBar.exists }, "⌘W on the empty pane closes the tab, no dialog")
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
        let filter = settings.textFields["settings.keyboard.filter"]
        XCTAssertTrue(filter.waitForExistence(timeout: 10), "filter field")
        filter.click()
        filter.typeText("Split Right")
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
        for title in ["New Tab", "Close Pane", "Close Tab", "Split Right", "Split Down", "Split Left", "Split Up",
                      "Focus Pane Left", "Focus Pane Right", "Zoom Pane", "Equalize Panes"] {
            XCTAssertTrue(layout.menuItems[title].exists, "\(title) in the Layout menu")
        }
        layout.typeKey(XCUIKeyboardKey.escape, modifierFlags: [])
    }
}

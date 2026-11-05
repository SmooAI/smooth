import XCTest

/// SMOODEV-4270: the browser pane in the real app against the mock fleet. The
/// mock's own HTTP API is the web server: any page will do, and its JSON shows
/// as text. The model and the URL rules are covered by BrowserPaneTests.
final class BrowserUITests: FlowUITestCase {
    override func setUpWithError() throws {
        try super.setUpWithError()
        try startMock()
        try launchApp()
    }

    private func element(_ id: String) -> XCUIElement { app.descendants(matching: .any)[id] }
    private var bar: XCUIElement { app.textFields["browser.url"] }
    private var barText: String { bar.value as? String ?? "" }

    private func selectSession() {
        waitForState("fs-1b8e05bb") { $0 == "working" }
        app.staticTexts["sidebar.title.fs-1b8e05bb"].click()
        XCTAssertTrue(waitUntil { self.label("pane.header").contains("th-1b8e05") }, label("pane.header"))
    }

    private func openPage(_ path: String = "/api/flow/sessions") -> String {
        let url = "http://\(addr)\(path)"
        app.typeKey("l", modifierFlags: [.command, .shift])
        XCTAssertTrue(bar.waitForExistence(timeout: 10), "⌘⇧L opens a browser pane; tree: \(dump())")
        app.typeText("\(url)\r")
        XCTAssertTrue(waitUntil { self.barText == url }, "bar: \(barText)")
        return url
    }

    func testCommandShiftLOpensAPageBesideTheTerminal() {
        selectSession()
        _ = openPage()
        XCTAssertTrue(app.staticTexts["pane.header"].exists, "the terminal pane is still there; tree: \(dump())")
        XCTAssertTrue(element("browser.back").exists)
        XCTAssertTrue(element("browser.forward").exists)
        XCTAssertTrue(element("browser.reload").exists)
    }

    func testSchemesOtherThanWebAreRefusedInTheBar() {
        selectSession()
        let url = openPage()
        for typed in ["javascript:alert(1)", "file:///etc/passwd"] {
            app.typeKey("l", modifierFlags: .command)
            app.typeText("\(typed)\r")
            XCTAssertTrue(waitUntil { self.label("browser.message").contains("Only http and https") }, "\(typed): \(dump())")
            app.typeKey(XCUIKeyboardKey.escape, modifierFlags: [])
            XCTAssertTrue(waitUntil { self.barText == url }, "Esc puts the page back in the bar: \(barText)")
        }
    }

    /// The sidebar must not take the pane the page is in.
    func testPickingASessionLeavesThePageAlone() {
        selectSession()
        let url = openPage()
        waitForState("fs-3041bbbb") { $0.hasPrefix("limit") }
        app.staticTexts["sidebar.title.fs-3041bbbb"].click()
        XCTAssertTrue(waitUntil { self.label("pane.header").contains("SMOODEV-3041") }, label("pane.header"))
        XCTAssertEqual(barText, url)
    }

    func testCommandWClosesThePaneWithNoQuestion() {
        selectSession()
        _ = openPage()
        app.typeKey("w", modifierFlags: .command)
        XCTAssertTrue(waitUntil { !self.bar.exists }, "the pane goes; tree: \(dump())")
        XCTAssertFalse(element("pane.close.cancel").exists, "a page is not a session")
        XCTAssertTrue(app.staticTexts["pane.header"].exists)
    }

    func testThePageComesBackAfterARelaunch() {
        selectSession()
        let url = openPage()
        app.typeKey("q", modifierFlags: .command)
        XCTAssertTrue(waitUntil(timeout: 30) { self.app.state == .notRunning }, "⌘Q quits")
        // Same temp HOME, and this time without the argument that shadows the layout.
        app.launchArguments = ["-onboarded", "YES", "-ApplePersistenceIgnoreState", "YES", "-NSQuitAlwaysKeepsWindows", "NO"]
        app.launch()
        XCTAssertTrue(bar.waitForExistence(timeout: 20), "the pane is back; tree: \(dump())")
        XCTAssertTrue(waitUntil { self.barText == url }, "bar: \(barText)")
    }
}

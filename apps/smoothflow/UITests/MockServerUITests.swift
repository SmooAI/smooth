import XCTest

/// The shell against the wireframe fleet (mock/server.mjs): what the user sees
/// for each session state, the inbox approval round-trip, and the settings panes.
final class MockServerUITests: FlowUITestCase {
    override func setUpWithError() throws {
        try super.setUpWithError()
        try startMock()
        try launchApp()
    }

    func testSidebarListsFixtureWithStateLabels() {
        waitForState("fs-d3e842aa") { $0 == "approve" }
        waitForState("fs-1b8e05bb") { $0 == "working" }
        waitForState("fs-3041bbbb") { $0.hasPrefix("limit") }
        waitForState("fs-3033cccc") { $0 == "done" }
        waitForState("fs-shell001") { $0 == "idle" }
        XCTAssertTrue(app.staticTexts["sidebar.header"].label.contains("sessions"))
        XCTAssertTrue(app.buttons["sidebar.needsYou"].exists, "needs-you pill for the permission request")
    }

    func testSelectingSessionShowsSurfaceAndHeader() {
        waitForState("fs-1b8e05bb") { $0 == "working" }
        app.staticTexts["sidebar.title.fs-1b8e05bb"].click()
        XCTAssertTrue(waitUntil { self.label("pane.header").contains("th-1b8e05") && self.label("pane.header").contains("working") }, label("pane.header"))
        XCTAssertTrue(label("center.path").contains("smooth-th-1b8e05"), label("center.path"))
    }

    func testInboxPermissionAllowFlipsSessionToWorking() {
        waitForState("fs-d3e842aa") { $0 == "approve" }
        app.typeKey("i", modifierFlags: .command)
        let card = app.otherElements["inbox.card.fs-d3e842aa"]
        XCTAssertTrue(card.waitForExistence(timeout: 10), "permission card in the inbox")
        app.buttons["inbox.allow.fs-d3e842aa"].click()
        // The mock answers flow.approve with flow.session{state: working}: the card
        // leaves NEEDS YOU and the sidebar row changes — the DOM, not the store.
        waitForState("fs-d3e842aa") { $0 == "working" }
        XCTAssertTrue(waitUntil { !card.exists })
    }

    func testSettingsPanesRender() {
        app.typeKey(",", modifierFlags: .command)
        let settings = app.windows["SmoothFlow Settings"]
        XCTAssertTrue(settings.waitForExistence(timeout: 10))
        XCTAssertTrue(settings.descendants(matching: .any)["settings.pane.permissions"].waitForExistence(timeout: 10), "Permissions pane is the first tab")
        let daemonTab = settings.radioButtons["Daemon"].exists ? settings.radioButtons["Daemon"] : settings.buttons["Daemon"]
        XCTAssertTrue(daemonTab.waitForExistence(timeout: 5), "Daemon tab")
        daemonTab.click()
        XCTAssertTrue(settings.descendants(matching: .any)["settings.pane.daemon"].waitForExistence(timeout: 10), "Daemon pane")
        XCTAssertTrue(settings.staticTexts["Restart daemon"].exists || settings.buttons["Restart daemon"].exists)
    }
}

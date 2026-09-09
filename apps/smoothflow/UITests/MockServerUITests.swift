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
        XCTAssertTrue(label("sidebar.header").contains("sessions"), label("sidebar.header"))
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
        XCTAssertTrue(app.windows["Inbox"].waitForExistence(timeout: 10), "⌘I opens the inbox window")
        // The card is a SwiftUI container (AX group), so match by any type.
        let card = app.windows["Inbox"].descendants(matching: .any)["inbox.card.fs-d3e842aa"]
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
        let daemonTab = settingsTab("Daemon", in: settings)
        XCTAssertTrue(daemonTab.waitForExistence(timeout: 5), "Daemon tab")
        daemonTab.click()
        XCTAssertTrue(settings.descendants(matching: .any)["settings.pane.daemon"].waitForExistence(timeout: 10), "Daemon pane")
        XCTAssertTrue(settings.staticTexts["Restart daemon"].exists || settings.buttons["Restart daemon"].exists)
    }

    /// Settings ▸ Phones (th-d98fde): the seeded pairing lists, "Pair a phone…"
    /// puts a QR on screen, and the mock's scan (3rd poll) lands as a paired row.
    func testPhonesPanePairsAgainstTheMock() {
        app.typeKey(",", modifierFlags: .command)
        let settings = app.windows["SmoothFlow Settings"]
        XCTAssertTrue(settings.waitForExistence(timeout: 10))
        let phonesTab = settingsTab("Phones", in: settings)
        XCTAssertTrue(phonesTab.waitForExistence(timeout: 5), "Phones tab")
        phonesTab.click()
        XCTAssertTrue(settings.descendants(matching: .any)["settings.pane.phones"].waitForExistence(timeout: 10), "Phones pane")
        XCTAssertTrue(settings.staticTexts["Brent’s Pixel"].waitForExistence(timeout: 10), "seeded pairing is listed")
        settings.buttons["settings.phones.pair"].click()
        XCTAssertTrue(settings.descendants(matching: .any)["settings.phones.qr"].waitForExistence(timeout: 10), "QR on screen")
        XCTAssertTrue(settings.staticTexts["Mock iPhone"].waitForExistence(timeout: 15), "the mock's scan lands as a paired row")
        XCTAssertFalse(settings.descendants(matching: .any)["settings.phones.qr"].exists, "QR leaves once paired")
        settings.buttons["settings.phones.revoke.phone-mock0000001"].click()
        XCTAssertTrue(waitUntil { !settings.staticTexts["Mock iPhone"].exists }, "revoked row disappears")
    }
}

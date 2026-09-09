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

    /// th-883ce9: Close on a finished card → confirm sheet → `flow.close`; the
    /// mock answers with `flow.session.removed`, so the card AND the sidebar row go.
    func testInboxCloseFinishedSessionRemovesIt() {
        waitForState("fs-3033cccc") { $0 == "done" }
        app.typeKey("i", modifierFlags: .command)
        let inbox = app.windows["Inbox"]
        XCTAssertTrue(inbox.waitForExistence(timeout: 10))
        let card = inbox.descendants(matching: .any)["inbox.finished.fs-3033cccc"]
        XCTAssertTrue(card.waitForExistence(timeout: 10), "finished card; tree: \(dump())")
        app.buttons["inbox.close.fs-3033cccc"].click()
        XCTAssertTrue(app.buttons["inbox.close.confirm"].waitForExistence(timeout: 10), "confirm sheet")
        XCTAssertTrue(app.checkBoxes["inbox.close.pearl"].exists, "pearl toggle for a row with a pearl")
        XCTAssertTrue(app.checkBoxes["inbox.close.worktree"].exists, "worktree toggle for a row in its own worktree")
        app.buttons["inbox.close.confirm"].click()
        XCTAssertTrue(waitUntil { !card.exists }, "card gone after flow.session.removed")
        XCTAssertTrue(waitUntil { !self.app.staticTexts["sidebar.state.fs-3033cccc"].exists }, "sidebar row gone")
    }

    /// th-883ce9: the mock refuses the unmerged fixture (`flow.error` with our
    /// seq as `ref`); the card shows the reason and Force close resends with force.
    func testInboxCloseRefusalThenForce() {
        waitForState("fs-3034dddd") { $0 == "done" }
        app.typeKey("i", modifierFlags: .command)
        let inbox = app.windows["Inbox"]
        XCTAssertTrue(inbox.waitForExistence(timeout: 10))
        let card = inbox.descendants(matching: .any)["inbox.finished.fs-3034dddd"]
        XCTAssertTrue(card.waitForExistence(timeout: 10), "finished card; tree: \(dump())")
        app.buttons["inbox.close.fs-3034dddd"].click()
        XCTAssertTrue(app.buttons["inbox.close.confirm"].waitForExistence(timeout: 10))
        app.buttons["inbox.close.confirm"].click()
        let refusal = app.staticTexts["inbox.close.refusal.fs-3034dddd"]
        XCTAssertTrue(refusal.waitForExistence(timeout: 10), "refusal lands on the card, not the rail; tree: \(dump())")
        XCTAssertTrue(label("inbox.close.refusal.fs-3034dddd").contains("not merged"), label("inbox.close.refusal.fs-3034dddd"))
        XCTAssertTrue(card.exists, "nothing touched: the row is still there")
        app.buttons["inbox.close.force.fs-3034dddd"].click()
        XCTAssertTrue(waitUntil { !card.exists }, "forced close removes the row")
        XCTAssertTrue(waitUntil { !self.app.staticTexts["sidebar.state.fs-3034dddd"].exists })
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

    /// Settings ▸ Phones (th-d98fde): the seeded pairing lists, "Pair a phone…"
    /// puts a QR on screen, and the mock's scan (3rd poll) lands as a paired row.
    func testPhonesPanePairsAgainstTheMock() {
        app.typeKey(",", modifierFlags: .command)
        let settings = app.windows["SmoothFlow Settings"]
        XCTAssertTrue(settings.waitForExistence(timeout: 10))
        let phonesTab = settings.radioButtons["Phones"].exists ? settings.radioButtons["Phones"] : settings.buttons["Phones"]
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

import XCTest

/// The shell against the real flow engine with `fake-claude` playing Claude
/// Code: steer → hooks → state, a hook-reported permission answered from the
/// inbox, and kill+resume relaunching with `--resume`. Every assertion reads
/// either the UI or the engine's own `/snapshot` — never app internals.
final class RealEngineUITests: FlowUITestCase {
    private var id = ""

    override func setUpWithError() throws {
        try super.setUpWithError()
        try startEngine()
        try launchApp()
        id = try newSession()
        XCTAssertTrue(app.staticTexts["sidebar.title.\(id)"].waitForExistence(timeout: 15), "new session shows in the sidebar")
        app.staticTexts["sidebar.title.\(id)"].click()
        waitForSnapshot(id, containing: "fake-claude ready")
    }

    private func steer(_ text: String) {
        let field = app.textFields["steer.field"]
        XCTAssertTrue(field.waitForExistence(timeout: 10))
        field.click()
        field.typeText(text + "\n")
    }

    func testSteerRoundTripsThroughHooksToIdle() {
        steer("/work hello")
        waitForSnapshot(id, containing: "worked: hello")
        waitForState(id) { $0 == "idle" }
    }

    func testPermissionRequestAllowedFromInbox() {
        steer("/perm")
        waitForState(id) { $0 == "approve" }
        app.typeKey("i", modifierFlags: .command)
        XCTAssertTrue(app.windows["Inbox"].waitForExistence(timeout: 10), "⌘I opens the inbox window")
        XCTAssertTrue(app.windows["Inbox"].descendants(matching: .any)["inbox.card.\(id)"].waitForExistence(timeout: 10), "hook-reported permission in the inbox")
        app.buttons["inbox.allow.\(id)"].click()
        // fake-claude prints the long-polled hook reply verbatim.
        waitForSnapshot(id, containing: "\"behavior\":\"allow\"")
        waitForState(id) { $0 != "approve" }
    }

    func testKillAndResumeRelaunchesWithResume() {
        waitForState(id) { $0 != "" }
        app.typeKey("r", modifierFlags: [.command, .option])
        waitForSnapshot(id, containing: "resume=1")
        XCTAssertTrue(waitUntil { self.label("center.path").contains("--resume") }, label("center.path"))
    }
}

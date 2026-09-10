import AppKit
import XCTest

/// Quit must be honored from every sender (th-6198bf): `NSRunningApplication.terminate()`
/// is the Quit Apple event — the same thing AppleScript's `quit` and the release
/// lane's "install over the running app" send. The app used to handle the event
/// and then sit in `applicationWillTerminate` forever when its child daemon
/// ignored TERM; a pid kill was the only way out.
final class QuitUITests: FlowUITestCase {
    /// The everyday case: connected to a backend the app did not spawn.
    func testQuitAppleEventExitsAgainstTheMock() throws {
        try startMock()
        try launchApp()
        try quitViaQuitAppleEvent()
    }

    /// The bug: spawn mode with a child daemon that ignores SIGTERM. Quit still
    /// completes within the bounded shutdown.
    func testQuitIsBoundedWhenTheChildDaemonIgnoresTerm() throws {
        try launchAppSpawningStubbornDaemon()
        let elapsed = try quitViaQuitAppleEvent()
        XCTAssertLessThan(elapsed, 12, "bounded: supervisor grace + app-side backstop, not forever")
        XCTAssertNil(DaemonManager_readPid(pidFile), "the stubborn daemon was SIGKILLed with the app")
    }

    /// Send the Quit Apple event to the instance this test launched and wait for it to exit.
    @discardableResult
    private func quitViaQuitAppleEvent() throws -> TimeInterval {
        // launchApp() already refused to run next to a real SmoothFlow.app, so the
        // only instance of the bundle id is ours.
        let target = try XCTUnwrap(NSRunningApplication.runningApplications(withBundleIdentifier: "ai.smoo.smoothflow").first, "our instance")
        let t0 = Date()
        XCTAssertTrue(target.terminate(), "the Quit Apple event was accepted")
        XCTAssertTrue(app.wait(for: .notRunning, timeout: 20), "the app exited on quit")
        return Date().timeIntervalSince(t0)
    }

    /// The pid file the app's supervisor writes for its child daemon.
    private var pidFile: URL { tmp.appendingPathComponent("home/.smooth/smoothflow-daemon.pid") }

    /// `DaemonManager.readPid` is in the app module (not importable from a UI test); same rule.
    private func DaemonManager_readPid(_ f: URL) -> pid_t? {
        guard let t = try? String(contentsOf: f, encoding: .utf8), let pid = pid_t(t.trimmingCharacters(in: .whitespacesAndNewlines)), pid > 1 else { return nil }
        return kill(pid, 0) == 0 ? pid : nil
    }
}

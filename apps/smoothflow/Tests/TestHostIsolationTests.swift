import AppKit
import XCTest
@testable import SmoothFlow

/// th-dccc80: the unit bundle is hosted inside SmoothFlow.app, so the real app
/// delegate runs on every `xcodebuild test`. It must stay inert — no window, no
/// daemon child, no `tmux kill-server`, nothing written under `~/.smooth` — and
/// the host process must not even be looking at the developer's HOME.
final class TestHostIsolationTests: XCTestCase {
    func testStartGateReadsTheXCTestVariables() {
        XCTAssertTrue(AppController.shouldStart(env: [:]), "a normal launch starts")
        XCTAssertTrue(AppController.shouldStart(env: ["SMOOTHFLOW_UI_TEST": "1", "SMOOTHFLOW_DAEMON_ADDR": "127.0.0.1:1"]), "the app under an XCUITest is a separate process and must start")
        for key in ["XCTestConfigurationFilePath", "XCTestBundlePath", "XCTestSessionIdentifier"] {
            XCTAssertFalse(AppController.shouldStart(env: [key: "x"]), "\(key) marks the test host")
        }
        XCTAssertTrue(AppController.shouldStart(env: ["XCTestConfigurationFilePath": "x", "SMOOTHFLOW_TEST_START": "1"]), "a test can opt in")
    }

    @MainActor
    func testHostedAppIsInert() throws {
        let delegate = try XCTUnwrap(NSApp.delegate as? AppDelegate, "the test host is the real app")
        XCTAssertFalse(AppController.shouldStart(env: ProcessInfo.processInfo.environment), "this process IS the test host")
        XCTAssertFalse(delegate.app.started)
        XCTAssertNil(delegate.app.mainWindow, "no window was built")
        XCTAssertEqual(delegate.app.daemon.status, "not started", "no daemon child")
        XCTAssertNil(delegate.app.daemon.endpoint)
        XCTAssertTrue(NSApp.windows.filter { $0.title == "SmoothFlow" }.isEmpty)
        // shutdown() before start() is the quit path of an inert host: it must
        // not reach tmux (`kill-server` on the developer's `smoothflow` socket).
        delegate.app.shutdown()
        XCTAssertFalse(delegate.app.didShutdown, "nothing to shut down")
    }

    func testHostHomeIsNotTheDevelopersAndStaysUntouched() {
        let home = FileManager.default.homeDirectoryForCurrentUser
        XCTAssertEqual(home.path, "/tmp/smoothflow-xctest-home", "the SmoothFlow scheme pins the test host's HOME (project.yml)")
        XCTAssertEqual(ProcessInfo.processInfo.environment["CFFIXED_USER_HOME"], home.path, "UserDefaults follow the same home")
        // Nothing in the app wrote under it: no daemon.addr (the child would),
        // no relay device id, no operator/flow db.
        let smooth = home.appendingPathComponent(".smooth")
        let written = (try? FileManager.default.contentsOfDirectory(atPath: smooth.path)) ?? []
        XCTAssertFalse(written.contains("daemon.addr"), "daemon.addr must never be written by the test host: \(written)")
        XCTAssertFalse(written.contains { $0.hasPrefix("smoothflow-") }, "no smoothflow-* state either: \(written)")
    }
}

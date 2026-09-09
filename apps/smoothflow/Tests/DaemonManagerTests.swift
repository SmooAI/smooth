import XCTest
@testable import SmoothFlow

/// The child supervisor and the bounded stop (th-6198bf), exercised for real:
/// `/bin/sh -c childSupervisor` around a scripted "daemon". Quit used to block
/// in `waitUntilExit()` for as long as the daemon took to die — forever, for
/// one that ignored TERM. These pin the escalation on both sides.
@MainActor
final class DaemonSupervisorTests: XCTestCase {
    private var tmp: URL!

    override func setUpWithError() throws {
        try super.setUpWithError()
        tmp = FileManager.default.temporaryDirectory.appendingPathComponent("smoothflow-sup-\(UUID().uuidString.prefix(8))")
        try FileManager.default.createDirectory(at: tmp, withIntermediateDirectories: true)
    }

    override func tearDown() {
        if let tmp { try? FileManager.default.removeItem(at: tmp) }
        super.tearDown()
    }

    /// A "daemon" script; `ignoresTerm` makes it shrug off SIGTERM like a wedged
    /// daemon. It touches `ready` once its trap is installed — a TERM in the
    /// first milliseconds would otherwise land before the trap and prove nothing.
    private var ready: URL { tmp.appendingPathComponent("ready") }
    private func fakeDaemon(ignoresTerm: Bool) throws -> String {
        let path = tmp.appendingPathComponent(ignoresTerm ? "stubborn" : "polite").path
        let body = "#!/bin/sh\n" + (ignoresTerm ? "trap '' TERM\n" : "") + ": > '\(ready.path)'\nwhile :; do sleep 1; done\n"
        try body.write(toFile: path, atomically: true, encoding: .utf8)
        try FileManager.default.setAttributes([.posixPermissions: 0o755], ofItemAtPath: path)
        return path
    }

    /// Launch the real supervisor around `daemon`, wait for its pid file.
    private func supervise(_ daemon: String) throws -> (Process, URL) {
        let pidFile = tmp.appendingPathComponent("daemon.pid")
        let p = Process()
        p.executableURL = URL(fileURLWithPath: "/bin/sh")
        p.arguments = ["-c", DaemonManager.childSupervisor, daemon, "operator", "--addr", "127.0.0.1:1"]
        p.environment = ["SMOOTHFLOW_DAEMON_PIDFILE": pidFile.path, "PATH": "/usr/bin:/bin"]
        p.standardOutput = FileHandle.nullDevice
        p.standardError = FileHandle.nullDevice
        try p.run()
        let deadline = Date().addingTimeInterval(5)
        while DaemonManager.readPid(pidFile) == nil || !FileManager.default.fileExists(atPath: ready.path), Date() < deadline {
            Thread.sleep(forTimeInterval: 0.05)
        }
        XCTAssertNotNil(DaemonManager.readPid(pidFile), "the supervisor records the daemon pid")
        XCTAssertTrue(FileManager.default.fileExists(atPath: ready.path), "the daemon is up (trap installed)")
        return (p, pidFile)
    }

    func testTermTakesAPoliteDaemonDownQuietly() throws {
        let (p, pidFile) = try supervise(try fakeDaemon(ignoresTerm: false))
        let daemon = try XCTUnwrap(DaemonManager.readPid(pidFile))
        let t0 = Date()
        XCTAssertTrue(DaemonManager.stopChild(p, daemonPid: daemon, grace: 5), "no escalation needed")
        XCTAssertLessThan(Date().timeIntervalSince(t0), 3, "a TERM-honoring tree is gone in about a second")
        XCTAssertFalse(p.isRunning)
        XCTAssertNotEqual(kill(daemon, 0), 0, "daemon reaped")
    }

    func testSupervisorEscalatesToKillWhenTheDaemonIgnoresTerm() throws {
        let (p, pidFile) = try supervise(try fakeDaemon(ignoresTerm: true))
        let daemon = try XCTUnwrap(DaemonManager.readPid(pidFile))
        let t0 = Date()
        // Grace longer than the supervisor's own: the supervisor must finish the
        // job itself (TERM, wait supervisorGraceSeconds, KILL) — the app-side
        // SIGKILL is the backstop, not the plan.
        let quiet = DaemonManager.stopChild(p, daemonPid: daemon, grace: TimeInterval(DaemonManager.supervisorGraceSeconds + 4))
        let elapsed = Date().timeIntervalSince(t0)
        XCTAssertGreaterThan(elapsed, 2, "the daemon really did ignore TERM: the supervisor had to wait out its grace")
        XCTAssertTrue(quiet, "the supervisor's own escalation ended the tree (\(elapsed)s)")
        XCTAssertLessThan(elapsed, TimeInterval(DaemonManager.supervisorGraceSeconds + 3))
        XCTAssertFalse(p.isRunning)
        XCTAssertNotEqual(kill(daemon, 0), 0, "stubborn daemon was SIGKILLed")
    }

    func testStopChildKillsASupervisorThatDoesNotExit() throws {
        // Worst case: the supervisor itself is stuck. The app-side bound still holds.
        let stuck = try fakeDaemon(ignoresTerm: true)
        let p = Process()
        p.executableURL = URL(fileURLWithPath: "/bin/sh")
        p.arguments = [stuck]
        p.standardOutput = FileHandle.nullDevice
        try p.run()
        let deadline = Date().addingTimeInterval(5)
        while !FileManager.default.fileExists(atPath: ready.path), Date() < deadline { Thread.sleep(forTimeInterval: 0.05) }
        let t0 = Date()
        XCTAssertFalse(DaemonManager.stopChild(p, daemonPid: nil, grace: 1), "TERM did not do it")
        XCTAssertLessThan(Date().timeIntervalSince(t0), 4)
        XCTAssertFalse(p.isRunning, "SIGKILLed")
    }

    func testStopChildOnAnExitedProcessIsANoOp() throws {
        let p = Process()
        p.executableURL = URL(fileURLWithPath: "/usr/bin/true")
        try p.run()
        p.waitUntilExit()
        XCTAssertTrue(DaemonManager.stopChild(p, daemonPid: nil, grace: 1))
    }

    func testReadPidShapes() throws {
        let f = tmp.appendingPathComponent("pid")
        XCTAssertNil(DaemonManager.readPid(f), "missing file")
        try "junk\n".write(to: f, atomically: true, encoding: .utf8)
        XCTAssertNil(DaemonManager.readPid(f))
        try "1\n".write(to: f, atomically: true, encoding: .utf8)
        XCTAssertNil(DaemonManager.readPid(f), "never launchd")
        try " \(ProcessInfo.processInfo.processIdentifier) \n".write(to: f, atomically: true, encoding: .utf8)
        XCTAssertEqual(DaemonManager.readPid(f), ProcessInfo.processInfo.processIdentifier)
        try "999999\n".write(to: f, atomically: true, encoding: .utf8)
        XCTAssertNil(DaemonManager.readPid(f), "a dead pid is not a pid")
    }

    func testSupervisorShapeIsWhatQuitReliesOn() {
        let s = DaemonManager.childSupervisor
        XCTAssertTrue(s.contains("SMOOTHFLOW_DAEMON_PIDFILE"), "records the daemon pid")
        XCTAssertTrue(s.contains("kill -9 \"$d\""), "escalates to SIGKILL")
        XCTAssertTrue(s.contains("-lt \(DaemonManager.supervisorGraceSeconds)"), "after the documented grace")
        XCTAssertTrue(s.contains("trap 'down; wait \"$d\"; exit 0' TERM INT"), "TERM/INT run the same escalation")
    }
}

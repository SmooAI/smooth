import XCTest
@testable import SmoothFlow

/// SMOODEV-4111: where ⌘N, a split and New Shell in Directory… open a shell.
final class ShellLaunchTests: XCTestCase {
    private let home = "/Users/me"
    private func s(_ id: String, worktree: String = "/repo") -> Session { Session(id: id, project: "/repo", worktree: worktree) }

    func testAPickedDirectoryWins() {
        let n = ShellLaunch.request(source: s("fs-1"), picked: "/picked", last: "/last", home: home) { _ in true }
        XCTAssertEqual(n.kind, "shell")
        XCTAssertEqual(n.worktree, "/picked")
        XCTAssertNil(n.cwdOf)
    }

    /// The focused pane's session goes by id: the engine reads its live cwd
    /// from tmux, so the app never sends the (stale) launch worktree.
    func testTheFocusedSessionsLiveCwdIsAskedOfTheEngine() {
        let n = ShellLaunch.request(source: s("fs-1", worktree: "/launched-here"), last: "/last", home: home) { _ in true }
        XCTAssertEqual(n.cwdOf, "fs-1")
        XCTAssertNil(n.worktree, "worktree would override cwd_of in the engine")
        XCTAssertEqual(n.fields["cwd_of"] as? String, "fs-1")
    }

    func testNothingFocusedFallsBackToTheLastDirectoryThenHome() {
        XCTAssertEqual(ShellLaunch.request(source: nil, last: "/last", home: home) { $0 == "/last" }.worktree, "/last")
        XCTAssertEqual(ShellLaunch.request(source: nil, last: "/gone", home: home) { _ in false }.worktree, home, "a deleted last dir is skipped")
        XCTAssertEqual(ShellLaunch.request(source: nil, last: nil, home: home).worktree, home)
        XCTAssertEqual(ShellLaunch.request(source: nil, picked: "", last: nil, home: home).worktree, home, "an empty pick is no pick")
    }

    /// `cwd_of` is additive: a frame without it is byte-for-byte what older
    /// engines and the phone already speak.
    func testCwdOfIsOffTheWireWhenUnset() {
        let plain = NewSession(kind: "shell", worktree: "/w")
        XCTAssertNil(plain.fields["cwd_of"])
        let json = String(decoding: ClientFrame.new(plain).encode(), as: UTF8.self)
        XCTAssertFalse(json.contains("cwd_of"), json)
    }

    func testLastDirectoryRemembersOnlyAbsolutePaths() throws {
        let d = try XCTUnwrap(UserDefaults(suiteName: "ShellLaunchTests-\(UUID().uuidString)"))
        XCTAssertNil(LastDirectory.load(d))
        LastDirectory.remember("/Users/me/dev/x", d)
        XCTAssertEqual(LastDirectory.load(d), "/Users/me/dev/x")
        LastDirectory.remember("relative", d)
        LastDirectory.remember(nil, d)
        XCTAssertEqual(LastDirectory.load(d), "/Users/me/dev/x")
    }
}

import XCTest
@testable import SmoothFlow

/// SMOODEV-4269: sessions on another machine over ssh.
final class RemoteTargetTests: XCTestCase {
    func testPlainDestinationsAreAccepted() {
        for ok in ["box", "dev-box", "box.tail1234.ts.net", "me@box", "my_user@10.0.0.2", "::1", "me@fe80::1%en0", " box "] {
            XCTAssertNil(RemoteTarget.problem(ok), ok)
        }
    }

    /// The argument-injection case: a value ssh would read as an option.
    func testAHostStartingWithADashIsRefused() {
        for bad in ["-oProxyCommand=x", "-J", "-", "me@-box", " -oProxyCommand=x"] {
            XCTAssertNotNil(RemoteTarget.problem(bad), bad)
            XCTAssertTrue(RemoteTarget.problem(bad)?.contains("-") == true, bad)
        }
        XCTAssertNil(RemoteTarget.from(host: "-oProxyCommand=x", cwd: ""))
    }

    func testHostileHostsAreRefused() {
        for bad in ["", "box;id", "box id", "$(id)", "`id`", "box|id", "ssh://box", "box/x", "a=b", "me@", "@box", "a@b@c", "bøx", String(repeating: "a", count: 256)] {
            XCTAssertNotNil(RemoteTarget.problem(bad), bad)
        }
    }

    func testAPasswordIsRefusedAndNeverRemembered() throws {
        XCTAssertTrue(RemoteTarget.problem("me:hunter2@box")?.contains("password") == true)
        XCTAssertFalse(RemoteTarget.problem("me:hunter2@box")?.contains("hunter2") == true)
        let suite = "RemoteTargetTests-\(UUID().uuidString)"
        let d = try XCTUnwrap(UserDefaults(suiteName: suite))
        defer { d.removePersistentDomain(forName: suite) }
        RecentHosts.remember("me:hunter2@box", d)
        RecentHosts.remember("-oProxyCommand=x", d)
        XCTAssertEqual(RecentHosts.load(d), [])
    }

    func testRecentHostsAreNewestFirstAndBounded() throws {
        let suite = "RemoteTargetTests-\(UUID().uuidString)"
        let d = try XCTUnwrap(UserDefaults(suiteName: suite))
        defer { d.removePersistentDomain(forName: suite) }
        for h in ["a", "b", "a", " c "] { RecentHosts.remember(h, d) }
        XCTAssertEqual(RecentHosts.load(d), ["c", "a", "b"])
        for i in 0..<20 { RecentHosts.remember("h\(i)", d) }
        XCTAssertEqual(RecentHosts.load(d).count, RecentHosts.limit)
    }

    func testABlankDirectoryIsTheRemoteLoginDirectory() {
        XCTAssertEqual(RemoteTarget.from(host: " box ", cwd: "  "), RemoteTarget(host: "box", cwd: nil))
        XCTAssertEqual(RemoteTarget.from(host: "box", cwd: "~/api"), RemoteTarget(host: "box", cwd: "~/api"))
        XCTAssertEqual(RemoteTarget(host: "box", cwd: "~/api").label, "box:~/api")
    }

    /// A remote row groups under its host, decodes from the engine's frame,
    /// and an older engine's row is simply local.
    func testASessionRowCarriesItsTarget() throws {
        let json = #"{"id":"fs-1","kind":"shell","title":"box","project":"ssh://me@box","worktree":"","remote":{"host":"me@box","cwd":"/srv"},"state":"idle"}"#
        let s = try JSONDecoder().decode(Session.self, from: Data(json.utf8))
        XCTAssertEqual(s.remote, RemoteTarget(host: "me@box", cwd: "/srv"))
        XCTAssertEqual(s.projectName, "me@box")
        let old = try JSONDecoder().decode(Session.self, from: Data(#"{"id":"fs-2","project":"/repo","worktree":"/repo"}"#.utf8))
        XCTAssertNil(old.remote)
    }

    func testTheFrameOnlyCarriesATargetWhenThereIsOne() {
        XCTAssertNil(NewSession(kind: "shell").fields["remote"], "older engines see the frame unchanged")
        let f = NewSession(kind: "claude", remote: RemoteTarget(host: "box", cwd: nil)).fields["remote"] as? [String: Any]
        XCTAssertEqual(f?["host"] as? String, "box")
        XCTAssertNil(f?["cwd"])
    }

    func testAShellOnAHostSendsNoLocalDirectory() {
        let n = ShellLaunch.request(source: Session(id: "fs-1"), remote: RemoteTarget(host: "box", cwd: "/srv"), last: "/last", home: "/Users/me") { _ in true }
        XCTAssertEqual(n.remote, RemoteTarget(host: "box", cwd: "/srv"))
        XCTAssertNil(n.worktree)
        XCTAssertNil(n.cwdOf)
    }

    /// A split asks by id, and the engine keeps it on the source's host.
    func testASplitOfARemotePaneGoesByItsSession() {
        let source = Session(id: "fs-1", project: "ssh://box", remote: RemoteTarget(host: "box"))
        let n = ShellLaunch.request(source: source, last: nil, home: "/Users/me")
        XCTAssertEqual(n.cwdOf, "fs-1")
        XCTAssertNil(n.remote)
        XCTAssertNil(n.worktree)
    }

    func testALostConnectionDoesNotReadAsAnApproval() throws {
        let frame = #"{"reason":"restorable","detail":"SSH to box failed"}"#
        let a = try JSONDecoder().decode(Attention.self, from: Data(frame.utf8))
        XCTAssertEqual(a.reason, .restorable)
        let s = Session(id: "fs-1", state: .needsYou, attention: a)
        XCTAssertEqual(Theme.stateLabel(s), "resume?")
    }

    /// The code isn't here: Diff and PR say so through the usual gate.
    func testARemoteRowHasNoDiffOrPRTab() {
        let s = Session(id: "fs-1", kind: "claude", project: "ssh://box", remote: RemoteTarget(host: "box"))
        let gate = CenterTabGate.of(session: s, packet: nil, harnesses: [])
        XCTAssertFalse(gate.diff)
        XCTAssertFalse(gate.pr)
    }

    func testThePromiseAboutSurvivalIsOnScreen() {
        for needle in ["ssh", "Quitting", "connection drops", "tmux", "resume"] {
            XCTAssertTrue(RemoteTarget.survivalNote.contains(needle), needle)
        }
    }

    func testAPaneHeaderPathNamesTheHost() {
        let r = Session(id: "fs-1", argv: ["claude"], remote: RemoteTarget(host: "box", cwd: "/srv"))
        XCTAssertEqual(r.whereItRuns(reportedPwd: "/Users/me"), "ssh box:/srv")
        let l = Session(id: "fs-2", worktree: "/repo")
        XCTAssertEqual(l.whereItRuns(reportedPwd: nil), "/repo")
        XCTAssertEqual(l.whereItRuns(reportedPwd: "/repo/x"), "/repo/x")
    }
}

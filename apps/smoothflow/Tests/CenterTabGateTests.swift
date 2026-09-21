import XCTest
@testable import SmoothFlow

/// th-68d10a: Diff/PR follow the worktree, Activity the harness's state source.
final class CenterTabGateTests: XCTestCase {
    private let harnesses = [
        HarnessInfo(name: "claude", stateSource: "hooks"),
        HarnessInfo(name: "codex", stateSource: "native"),
        HarnessInfo(name: "aider", stateSource: "scrape"),
    ]

    private func packet(branch: String?, head: String?) -> Handoff.Packet {
        Handoff.Packet(worktree: "/w", branch: branch, head: head, dirty: [], agentSessionId: nil, next: nil)
    }

    func testNothingFocusedAllowsOnlyTheTerminal() {
        let g = CenterTabGate.of(session: nil, packet: nil, harnesses: harnesses)
        XCTAssertEqual(g, .nothingFocused)
        XCTAssertTrue(g.allows(.terminal))
        XCTAssertFalse(g.allows(.diff)); XCTAssertFalse(g.allows(.pr)); XCTAssertFalse(g.allows(.activity))
    }

    func testAShellInAWorktreeGetsDiffAndPRButNoActivity() {
        let shell = Session(id: "s", kind: "shell", worktree: "/w", branch: "th-1-x")
        let g = CenterTabGate.of(session: shell, packet: packet(branch: "th-1-x", head: "abc1234"), harnesses: harnesses)
        XCTAssertTrue(g.diff, "a shell in a worktree has a diff")
        XCTAssertTrue(g.pr, "…and a branch to hang a PR on")
        XCTAssertEqual(g.activity, .none, "a shell reports no activity")
        XCTAssertFalse(g.allows(.activity))
    }

    func testAShellOutsideARepoGetsNeitherDiffNorPR() {
        let shell = Session(id: "s", kind: "shell", worktree: "/Users/me", branch: nil)
        let g = CenterTabGate.of(session: shell, packet: packet(branch: nil, head: nil), harnesses: harnesses)
        XCTAssertFalse(g.diff); XCTAssertFalse(g.pr)
        XCTAssertEqual(g.whyNot(.diff), "Not in a git repository")
    }

    func testAShellInTheMainCheckoutStillShowsDiffAndAnEmptyPR() {
        // Decided edge case: uncommitted changes on main are worth seeing.
        let shell = Session(id: "s", kind: "shell", worktree: "/dev/smooth", branch: "main")
        let g = CenterTabGate.of(session: shell, packet: packet(branch: "main", head: "abc1234"), harnesses: harnesses)
        XCTAssertTrue(g.diff); XCTAssertTrue(g.pr)
    }

    func testDiffIsIndependentOfSessionKind() {
        for kind in ["shell", "claude", "codex", "aider", "unknown-harness"] {
            let s = Session(id: "s", kind: kind, worktree: "/w", branch: "b")
            XCTAssertTrue(CenterTabGate.of(session: s, packet: packet(branch: "b", head: "abc1234"), harnesses: harnesses).diff, kind)
            XCTAssertFalse(CenterTabGate.of(session: s, packet: packet(branch: nil, head: nil), harnesses: harnesses).diff, kind)
        }
    }

    func testDetachedHeadHasDiffButNoPR() {
        let s = Session(id: "s", kind: "claude", worktree: "/w", branch: nil)
        let g = CenterTabGate.of(session: s, packet: packet(branch: "HEAD", head: "abc1234"), harnesses: harnesses)
        XCTAssertTrue(g.diff)
        XCTAssertFalse(g.pr)
        XCTAssertEqual(g.whyNot(.pr), "No branch (detached HEAD)")
    }

    func testBeforeThePacketArrivesTheRowBranchStandsIn() {
        let inWorktree = Session(id: "s", kind: "shell", worktree: "/w", branch: "b")
        XCTAssertTrue(CenterTabGate.of(session: inWorktree, packet: nil, harnesses: harnesses).diff, "no flicker from greyed-out to enabled")
        let bare = Session(id: "s", kind: "shell", worktree: "/Users/me", branch: nil)
        XCTAssertFalse(CenterTabGate.of(session: bare, packet: nil, harnesses: harnesses).diff)
    }

    func testThePacketOverridesTheRowOnceItArrives() {
        // A stale row branch doesn't beat the packet saying there's no HEAD.
        let s = Session(id: "s", kind: "claude", worktree: "/gone", branch: "b")
        XCTAssertFalse(CenterTabGate.of(session: s, packet: packet(branch: "b", head: nil), harnesses: harnesses).diff)
    }

    func testActivityFollowsTheManifestStateSource() {
        XCTAssertEqual(CenterTabGate.activityDepth(kind: "claude", harnesses: harnesses), .full)
        XCTAssertEqual(CenterTabGate.activityDepth(kind: "codex", harnesses: harnesses), .full)
        XCTAssertEqual(CenterTabGate.activityDepth(kind: "aider", harnesses: harnesses), .thin)
        XCTAssertEqual(CenterTabGate.activityDepth(kind: "shell", harnesses: harnesses), .none)
        XCTAssertEqual(CenterTabGate.activityDepth(kind: "unlisted", harnesses: harnesses), .full, "unknown keeps the full view")
    }

    func testResolveFallsBackToTheTerminal() {
        let g = CenterTabGate(diff: false, pr: false, activity: .none)
        XCTAssertEqual(g.resolve(.diff), .terminal)
        XCTAssertEqual(g.resolve(.terminal), .terminal)
        XCTAssertEqual(CenterTabGate(diff: true, pr: true, activity: .thin).resolve(.activity), .activity)
    }

    func testUsableBranch() {
        XCTAssertNil(CenterTabGate.usableBranch(nil))
        XCTAssertNil(CenterTabGate.usableBranch(""))
        XCTAssertNil(CenterTabGate.usableBranch("HEAD"))
        XCTAssertEqual(CenterTabGate.usableBranch(" main\n"), "main")
    }

    // ── the Diff tab's base ────────────────────────────────────────────────

    func testDiffBaseIsTheFirstRefWithAMergeBase() {
        let base = DiffPlan.base { ref in ref == "origin/main" ? "deadbeef\n" : nil }
        XCTAssertEqual(base.ref, "origin/main")
        XCTAssertEqual(base.sha, "deadbeef")
    }

    func testDiffBaseFallsBackToHEADWithoutADefaultBranch() {
        let base = DiffPlan.base { _ in nil }
        XCTAssertEqual(base.ref, "HEAD"); XCTAssertEqual(base.sha, "HEAD")
        XCTAssertEqual(DiffPlan.base { _ in "fatal: not a valid object name" }.ref, "HEAD")
    }

    func testDiffRenderNamesTheBase() {
        let s = DiffPlan.render(status: " M a.swift\n", baseRef: "origin/main", diff: "diff --git a b")
        XCTAssertTrue(s.contains("# git status --short\n M a.swift"))
        XCTAssertTrue(s.contains("merge base with origin/main"))
        XCTAssertTrue(DiffPlan.render(status: "", baseRef: "HEAD", diff: "").contains("(no changes)"))
    }
}

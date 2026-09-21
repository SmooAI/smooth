import XCTest
@testable import SmoothFlow

/// th-fe75ca: the close-out decision, apart from any AppKit presentation.
/// Everything the sheet says about what it is going to destroy is decided here.
final class SessionCloseTests: XCTestCase {
    private func session(_ o: (inout Session) -> Void = { _ in }) -> Session {
        var s = Session(id: "fs-1", title: "e2e flake", project: "/p", worktree: "/p-th-abc123", branch: "th-abc123-e2e",
                        pearlId: "th-abc123", state: .done)
        o(&s)
        return s
    }

    private func handoff(dirty: [String] = [], branch: String? = "th-abc123-e2e", pearlTitle: String? = "Fix the e2e flake") -> Handoff {
        Handoff(pearl: Handoff.Pearl(id: "th-abc123", title: pearlTitle, status: "in_progress", priority: 2, labels: nil),
                handoff: Handoff.Packet(worktree: "/p-th-abc123", branch: branch, head: "a91c0e2", dirty: dirty, agentSessionId: nil, next: nil),
                checkpoints: nil, blocks: nil, pr: nil)
    }

    func testFinishedSessionOffersBothActionsAndNamesWhatItDestroys() {
        let p = SessionClose.decide(session: session(), handoff: handoff(dirty: []))
        XCTAssertFalse(p.isLive)
        XCTAssertNil(p.liveWarning)
        XCTAssertEqual(p.confirmTitle, "Close session")
        XCTAssertEqual(p.pearlLabel, "Close pearl th-abc123")
        XCTAssertEqual(p.pearlDetail, "Fix the e2e flake", "the pearl is named, not just its id")
        XCTAssertTrue(p.defaultClosePearl)
        XCTAssertEqual(p.worktreeLabel, "Remove worktree /p-th-abc123")
        XCTAssertEqual(p.branchLabel, "and delete branch th-abc123-e2e", "the branch is destroyed too — say so")
        XCTAssertTrue(p.defaultRemoveWorktree)
        XCTAssertEqual(p.dirtyCount, 0)
        XCTAssertNil(p.dirtyLabel)
    }

    /// The complaint that started this pearl: a running session could not be
    /// closed at all. It can — and the sheet says the agent gets killed first.
    func testLiveSessionIsCloseableAndSaysItWillBeKilled() {
        let p = SessionClose.decide(session: session { $0.state = .working }, handoff: handoff())
        XCTAssertTrue(p.isLive)
        XCTAssertEqual(p.confirmTitle, "Kill and close")
        XCTAssertTrue(p.liveWarning?.contains("kills it first") == true, p.liveWarning ?? "nil")
    }

    func testNeedsYouCountsAsLive() {
        XCTAssertTrue(SessionClose.decide(session: session { $0.state = .needsYou }, handoff: nil).isLive)
        XCTAssertFalse(SessionClose.decide(session: session { $0.state = .dead }, handoff: nil).isLive)
    }

    func testDirtyCountIsShownAndWarnsTheCloseWillBeRefused() {
        let p = SessionClose.decide(session: session(), handoff: handoff(dirty: ["a.rs", "Cargo.lock"]))
        XCTAssertEqual(p.dirtyCount, 2)
        XCTAssertEqual(p.dirtyLabel, "2 uncommitted files right now — a plain close will be refused.")
        XCTAssertEqual(SessionClose.decide(session: session(), handoff: handoff(dirty: ["a.rs"])).dirtyLabel,
                       "1 uncommitted file right now — a plain close will be refused.")
    }

    /// No cached `merged` flag: the engine reveals merged state by refusing,
    /// and a second source of truth here could disagree at the close.
    func testPlanCarriesNoMergedFlag() {
        let mirror = Mirror(reflecting: SessionClose.decide(session: session(), handoff: handoff()))
        XCTAssertFalse(mirror.children.contains { $0.label?.lowercased().contains("merged") == true },
                       "merged state belongs to the engine's refusal, not to a cached flag")
    }

    func testMainCheckoutKeepsItsWorktreeAndOffersNoToggle() {
        let p = SessionClose.decide(session: session { $0.worktree = "/p" }, handoff: handoff(dirty: ["a.rs"]))
        XCTAssertFalse(p.hasOwnWorktree)
        XCTAssertNil(p.worktreeLabel)
        XCTAssertNil(p.branchLabel)
        XCTAssertNil(p.dirtyLabel, "nothing is being removed, so the dirty warning would be noise")
        XCTAssertFalse(p.defaultRemoveWorktree)
        XCTAssertEqual(p.worktreeNote, "Main checkout — the worktree is kept.")
    }

    func testSessionWithoutAPearlSaysSoInsteadOfOfferingTheToggle() {
        let p = SessionClose.decide(session: session { $0.pearlId = nil }, handoff: nil)
        XCTAssertNil(p.pearlLabel)
        XCTAssertEqual(p.pearlNote, "No pearl on this session.")
        XCTAssertFalse(p.defaultClosePearl)
    }

    /// A shell row on the main checkout with no pearl: both toggles gone, and
    /// the sheet is still coherent.
    func testShellSessionHasNothingToDestroyButTheRow() {
        let s = Session(id: "fs-shell", kind: "shell", title: "zsh", project: "", worktree: "/Users/x", state: .idle)
        let p = SessionClose.decide(session: s, handoff: nil)
        XCTAssertNil(p.pearlLabel)
        XCTAssertNil(p.worktreeLabel)
        XCTAssertEqual(p.title, "Close zsh")
        XCTAssertTrue(p.isLive)
    }

    /// The branch falls back to the handoff packet when the row never carried one.
    func testBranchFallsBackToTheHandoffPacket() {
        let p = SessionClose.decide(session: session { $0.branch = nil }, handoff: handoff(branch: "th-abc123-e2e"))
        XCTAssertEqual(p.branchLabel, "and delete branch th-abc123-e2e")
        XCTAssertNil(SessionClose.decide(session: session { $0.branch = nil }, handoff: handoff(branch: nil)).branchLabel)
    }

    func testTitleUsesThePearlPrefixedLabel() {
        XCTAssertEqual(SessionClose.decide(session: session(), handoff: nil).title, "Close th-abc123 e2e flake")
    }

    func testHasOwnWorktree() {
        XCTAssertTrue(SessionClose.hasOwnWorktree(Session(id: "x", project: "/p", worktree: "/p-wt")))
        XCTAssertFalse(SessionClose.hasOwnWorktree(Session(id: "x", project: "/p", worktree: "/p")), "the main checkout is never offered for removal")
        XCTAssertFalse(SessionClose.hasOwnWorktree(Session(id: "x", project: "", worktree: "")))
        XCTAssertFalse(SessionClose.hasOwnWorktree(Session(id: "x", project: "", worktree: "/Users/x")),
                       "a projectless shell row must never be offered 'remove this directory'")
    }
}

import XCTest
@testable import SmoothFlow

@MainActor
final class FlowStoreTests: XCTestCase {
    private func s(_ id: String, _ state: SessionState = .working, project: String = "/dev/smooth", kind: String = "claude",
                   attention: Attention? = nil, unread: Bool = false) -> Session {
        Session(id: id, kind: kind, title: id, project: project, state: state, attention: attention, unread: unread)
    }

    func testHelloReplacesFleetAndFocusesFirst() {
        let store = FlowStore()
        XCTAssertEqual(store.apply(.hello(daemon: DaemonInfo(version: "1", machineLabel: "m"), sessions: [s("a"), s("b")])), [.connected])
        XCTAssertEqual(store.order, ["a", "b"])
        XCTAssertEqual(store.focusedId, "a")
        XCTAssertTrue(store.connection.isConnected)
        // A reconnect hello drops sessions the engine no longer has and keeps focus valid.
        store.focusedId = "b"
        store.apply(.hello(daemon: DaemonInfo(version: "1", machineLabel: "m"), sessions: [s("c")]))
        XCTAssertEqual(store.order, ["c"])
        XCTAssertEqual(store.focusedId, "c")
    }

    /// th-0f6126: pickers render the engine's harness list in its order, never
    /// a hidden one; `flow.harnesses` replaces it; the pure ordering helpers.
    func testHarnessListOrderHidingAndDisplayNames() {
        let store = FlowStore()
        let h = [HarnessInfo(name: "th-code", displayName: "th code", installed: true, stateSource: "native", orderIndex: 0),
                 HarnessInfo(name: "claude", displayName: "Claude Code", installed: true, orderIndex: 1),
                 HarnessInfo(name: "codex", displayName: "Codex", installed: false, hidden: true, orderIndex: 2, reason: "`codex` not found on PATH")]
        store.apply(.hello(daemon: DaemonInfo(version: "1", machineLabel: "m"), sessions: [], harnesses: h))
        XCTAssertEqual(store.harnesses.map(\.name), ["th-code", "claude"], "hidden dropped, order kept")
        XCTAssertEqual(store.displayName(forKind: "th-code"), "th code")
        XCTAssertEqual(store.displayName(forKind: "shell"), "Shell")
        XCTAssertEqual(store.displayName(forKind: "codex"), "codex", "a hidden kind falls back to the raw name")
        XCTAssertEqual(store.apply(.harnesses([h[1]])), [])
        XCTAssertEqual(store.harnesses.map(\.name), ["claude"])
        XCTAssertEqual(HarnessPicker.defaultKind(store), "claude")
        store.apply(.harnesses([]))
        XCTAssertEqual(HarnessPicker.defaultKind(store), "shell")
        XCTAssertEqual(HarnessPicker.defaultKind(store, includeShell: false), "claude")
        XCTAssertEqual(FanOutSheet.defaultCandidates(h).map(\.label), ["th-code", "claude · opus 5", "claude · fable 5.1"], "one per installed harness, in order")
        // Pure ordering helpers behind Settings ▸ Harnesses.
        XCTAssertEqual(HarnessOrdering.moved(["a", "b", "c"], at: 1, by: -1), ["b", "a", "c"])
        XCTAssertEqual(HarnessOrdering.moved(["a", "b", "c"], at: 0, by: -1), ["a", "b", "c"], "off the top is a no-op")
        XCTAssertEqual(HarnessOrdering.moved(["a", "b", "c"], at: 2, by: 1), ["a", "b", "c"], "off the bottom is a no-op")
        XCTAssertEqual(HarnessOrdering.toggled(["x"], "y"), ["x", "y"])
        XCTAssertEqual(HarnessOrdering.toggled(["x", "y"], "x"), ["y"])
        XCTAssertEqual(HarnessOrdering.visible(h).map(\.name), ["th-code", "claude"])
    }

    func testSessionUpsertKeepsOrderAndEmitsAttentionOnce() {
        let store = FlowStore()
        store.apply(.hello(daemon: DaemonInfo(version: "1", machineLabel: ""), sessions: [s("a"), s("b")]))
        let perm = Attention(reason: .permission, detail: "git push", requestId: "r1")
        let needs = s("b", .needsYou, attention: perm, unread: true)
        XCTAssertEqual(store.apply(.session(needs)), [.attention(needs)])
        XCTAssertEqual(store.order, ["a", "b"], "update must not move the row")
        XCTAssertEqual(store.apply(.session(needs)), [], "same attention again is not a new alert")
        let other = s("b", .needsYou, attention: Attention(reason: .permission, detail: "rm -rf", requestId: "r2"))
        XCTAssertEqual(store.apply(.session(other)), [.attention(other)], "a different request is")
        let new = s("z", .starting)
        XCTAssertEqual(store.apply(.session(new)), [])
        XCTAssertEqual(store.order, ["a", "b", "z"])
    }

    func testFinishedEffectFiresOnTransitionOnly() {
        let store = FlowStore()
        store.apply(.session(s("a")))
        let done = s("a", .done)
        XCTAssertEqual(store.apply(.session(done)), [.finished(done)])
        XCTAssertEqual(store.apply(.session(done)), [])
        let dead = s("a", .dead)
        XCTAssertEqual(store.apply(.session(dead)), [.finished(dead)])
    }

    func testRemovedFixesFocusAndFanoutMembership() {
        let store = FlowStore()
        let fo = FanOut(id: "fo", prompt: "p")
        var a = s("a"), b = s("b")
        a.fanOutId = "fo"; b.fanOutId = "fo"
        store.apply(.fanout(fo, candidates: [a, b]))
        store.focusedId = "b"
        store.apply(.sessionRemoved(id: "b"))
        XCTAssertEqual(store.order, ["a"])
        XCTAssertEqual(store.focusedId, "a")
        XCTAssertEqual(store.fanOutCandidates["fo"], ["a"])
        XCTAssertEqual(store.fanOut(for: store.sessions["a"]!)?.1.map(\.id), ["a"])
    }

    func testOutputOnlyForKnownSessionsAndNonEmpty() {
        let store = FlowStore()
        store.apply(.session(s("a")))
        let data = Data("x".utf8)
        XCTAssertEqual(store.apply(.output(id: "a", seq: 1, data: data)), [.output(id: "a", data: data)])
        XCTAssertEqual(store.apply(.output(id: "ghost", seq: 1, data: data)), [])
        XCTAssertEqual(store.apply(.output(id: "a", seq: 2, data: Data())), [])
    }

    func testAttentionFrameIsIdempotentWithSession() {
        let store = FlowStore()
        store.apply(.session(s("a")))
        let a = Attention(reason: .question, detail: "which?")
        XCTAssertEqual(store.apply(.attention(id: "a", attention: a)).count, 1)
        XCTAssertEqual(store.apply(.attention(id: "a", attention: a)), [], "same attention twice → one alert")
        XCTAssertEqual(store.apply(.attention(id: "a", attention: nil)), [])
        XCTAssertNil(store.sessions["a"]?.attention)
        XCTAssertEqual(store.apply(.attention(id: "nope", attention: a)), [])
    }

    func testGroupingCountsAndInbox() {
        let store = FlowStore()
        store.apply(.hello(daemon: DaemonInfo(version: "1", machineLabel: ""), sessions: [
            s("a", .working, project: "/x/smooth"),
            s("b", .needsYou, project: "/x/smooai", attention: Attention(reason: .permission)),
            s("c", .limited, project: "/x/smooth", attention: Attention(reason: .usageLimit)),
            s("d", .done, project: "/x/smooai"),
            s("e", .idle, project: "/x/smooth", attention: Attention(reason: .held, pid: 1)),
            s("f", .idle, project: "", kind: "shell"),
        ]))
        XCTAssertEqual(store.grouped.map(\.project), ["smooth", "smooai", "shells"])
        XCTAssertEqual(store.grouped[0].sessions.map(\.id), ["a", "c", "e"])
        let c = store.counts
        XCTAssertEqual([c.working, c.needsYou, c.done, c.idle], [1, 3, 1, 2])
        XCTAssertEqual(store.needsYou.map(\.id), ["b", "c", "e"], "permission, limit and held all need a human")
        XCTAssertEqual(store.finished.map(\.id), ["d"])
        XCTAssertEqual(store.working.map(\.id), ["a"])
    }

    func testFocusByIndexAndMarkRead() {
        let store = FlowStore()
        store.apply(.hello(daemon: DaemonInfo(version: "1", machineLabel: ""), sessions: [s("a"), s("b", unread: true)]))
        XCTAssertEqual(store.session(atIndex: 1)?.id, "b")
        XCTAssertNil(store.session(atIndex: 9))
        XCTAssertEqual(store.unreadCount, 1)
        store.markReadLocally("b")
        XCTAssertEqual(store.unreadCount, 0)
    }

    func testErrorFrameSurfacesMessage() {
        let store = FlowStore()
        XCTAssertEqual(store.apply(.error(ref: nil, code: "held", message: "pid 5 owns it")), [.error(ref: nil, message: "held: pid 5 owns it")])
        XCTAssertEqual(store.lastError, "held: pid 5 owns it")
        XCTAssertEqual(store.apply(.unknown(type: "flow.x")), [])
    }

    /// th-883ce9: the engine's `ref` (our `seq`) survives into the effect so a
    /// refused `flow.close` can find its card; a successful close is just
    /// `flow.session.removed`, which drops the row with no effect.
    func testErrorRefIsKeptAndRemovalDropsRow() {
        let store = FlowStore()
        store.apply(.hello(daemon: DaemonInfo(version: "1", machineLabel: ""), sessions: [s("a", .done), s("b")]))
        XCTAssertEqual(store.apply(.error(ref: 3, code: "refused", message: "not merged")), [.error(ref: 3, message: "refused: not merged")])
        XCTAssertEqual(store.finished.map(\.id), ["a"])
        XCTAssertEqual(store.apply(.sessionRemoved(id: "a")), [])
        XCTAssertEqual(store.finished, [])
        XCTAssertEqual(store.order, ["b"])
        XCTAssertTrue(CloseSessionSheet.hasOwnWorktree(Session(id: "x", project: "/p", worktree: "/p-wt")))
        XCTAssertFalse(CloseSessionSheet.hasOwnWorktree(Session(id: "x", project: "/p", worktree: "/p")), "the main checkout is never offered for removal")
        XCTAssertFalse(CloseSessionSheet.hasOwnWorktree(Session(id: "x", project: "", worktree: "")))
    }
}

@MainActor
final class FlowStoreIntegrationTests: XCTestCase {
    private func s(_ id: String) -> Session { Session(id: id, kind: "claude", title: id, project: "/p", state: .working) }
    private func ev(_ sid: String, _ n: Int, kind: String = "PostToolUse") -> FlowEvent {
        FlowEvent(sessionId: sid, eventId: "e\(n)", at: "2026-09-08T00:00:0\(n % 10)Z", kind: kind, text: "t\(n)")
    }

    func testHelloFocusesTheFirstLiveSessionNotADeadOne() {
        let store = FlowStore()
        var dead = s("dead"); dead.state = .dead
        store.apply(.hello(daemon: DaemonInfo(version: "1", machineLabel: ""), sessions: [dead, s("live")]))
        XCTAssertEqual(store.focusedId, "live")
        XCTAssertFalse(dead.isLive)
        XCTAssertTrue(s("live").isLive)
    }

    func testHelloEmitsConnectedSoSurfacesReattach() {
        let store = FlowStore()
        XCTAssertEqual(store.apply(.hello(daemon: DaemonInfo(version: "1", machineLabel: "m"), sessions: [s("a")])), [.connected])
    }

    func testEventsAccumulateDedupeAndCap() {
        let store = FlowStore()
        store.apply(.hello(daemon: DaemonInfo(version: "1", machineLabel: ""), sessions: [s("a")]))
        XCTAssertEqual(store.apply(.event(ev("a", 1))), [])
        XCTAssertEqual(store.apply(.event(ev("a", 1))), [], "a redelivered event is not appended twice")
        XCTAssertEqual(store.events["a"]?.count, 1)
        XCTAssertEqual(store.apply(.event(ev("ghost", 1))), [], "events for unknown sessions are dropped")
        XCTAssertNil(store.events["ghost"])
        for n in 2...(FlowStore.eventCap + 10) { store.apply(.event(ev("a", n))) }
        XCTAssertEqual(store.events["a"]?.count, FlowStore.eventCap)
        XCTAssertEqual(store.events["a"]?.first?.eventId, "e11", "oldest rows fall off")
        store.apply(.sessionRemoved(id: "a"))
        XCTAssertNil(store.events["a"])
    }

    func testNewPidMeansRelaunchedSoTheSurfaceReattaches() {
        let store = FlowStore()
        var a = s("a"); a.pid = 100
        store.apply(.hello(daemon: DaemonInfo(version: "1", machineLabel: ""), sessions: [a]))
        XCTAssertEqual(store.apply(.session(a)), [], "same pid, nothing")
        a.pid = 200
        XCTAssertEqual(store.apply(.session(a)), [.relaunched(id: "a")], "the engine resumed it under a new pid")
        a.pid = nil
        XCTAssertEqual(store.apply(.session(a)), [], "pid cleared (dying) is not a relaunch")
        a.pid = 300; a.state = .done
        XCTAssertEqual(store.apply(.session(a)), [.finished(a)], "a terminal state never re-attaches")
    }

    func testHandoffPushBecomesAnEffectOnlyForKnownSessions() {
        let store = FlowStore()
        store.apply(.hello(daemon: DaemonInfo(version: "1", machineLabel: ""), sessions: [s("a")]))
        let h = Handoff(pearl: nil, handoff: nil, checkpoints: nil, blocks: nil, pr: Handoff.PR(number: 1, url: nil, ci: nil))
        XCTAssertEqual(store.apply(.handoff(id: "a", h)), [.handoff(id: "a", h)])
        XCTAssertEqual(store.apply(.handoff(id: "zz", h)), [])
    }

    func testReconnectHelloDropsEventsOfVanishedSessions() {
        let store = FlowStore()
        store.apply(.hello(daemon: DaemonInfo(version: "1", machineLabel: ""), sessions: [s("a"), s("b")]))
        store.apply(.event(ev("a", 1)))
        store.apply(.event(ev("b", 1)))
        store.apply(.hello(daemon: DaemonInfo(version: "1", machineLabel: ""), sessions: [s("b")]))
        XCTAssertNil(store.events["a"])
        XCTAssertEqual(store.events["b"]?.count, 1)
    }
}

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
        XCTAssertEqual(store.apply(.hello(daemon: DaemonInfo(version: "1", machineLabel: "m"), sessions: [s("a"), s("b")])), [])
        XCTAssertEqual(store.order, ["a", "b"])
        XCTAssertEqual(store.focusedId, "a")
        XCTAssertTrue(store.connection.isConnected)
        // A reconnect hello drops sessions the engine no longer has and keeps focus valid.
        store.focusedId = "b"
        store.apply(.hello(daemon: DaemonInfo(version: "1", machineLabel: "m"), sessions: [s("c")]))
        XCTAssertEqual(store.order, ["c"])
        XCTAssertEqual(store.focusedId, "c")
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
        XCTAssertEqual(store.apply(.error(ref: nil, code: "held", message: "pid 5 owns it")), [.error("held: pid 5 owns it")])
        XCTAssertEqual(store.lastError, "held: pid 5 owns it")
        XCTAssertEqual(store.apply(.unknown(type: "flow.x")), [])
    }
}

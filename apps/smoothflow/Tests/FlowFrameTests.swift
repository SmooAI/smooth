import XCTest
@testable import SmoothFlow

/// Every `flow.*` type from smoothflow-protocol.md decodes; unknown types are ignored.
final class FlowFrameTests: XCTestCase {
    private func decode(_ json: String) throws -> FlowFrame { try FlowFrame.decode(Data(json.utf8)) }

    func testHello() throws {
        let f = try decode(#"{"channel":"flow","type":"flow.hello","daemon":{"version":"1.2","machine_label":"marvin"},"sessions":[{"id":"fs-1","kind":"claude","title":"t","project":"/p","worktree":"/w","branch":"b","pearl_id":"th-1","argv":["claude"],"state":"working","created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z","unread":true}]}"#)
        guard case let .hello(daemon, sessions) = f else { return XCTFail("\(f)") }
        XCTAssertEqual(daemon.machineLabel, "marvin")
        XCTAssertEqual(sessions.count, 1)
        XCTAssertEqual(sessions[0].pearlId, "th-1")
        XCTAssertEqual(sessions[0].state, .working)
        XCTAssertTrue(sessions[0].unread)
    }

    func testSessionWithAttentionAndArgvAsJsonText() throws {
        let f = try decode(#"{"type":"flow.session","session":{"id":"fs-2","state":"needs_you","argv":"[\"claude\",\"--resume\",\"x\"]","attention":{"reason":"permission","detail":{"command":"git push"},"request_id":42,"resume_at":null}}}"#)
        guard case let .session(s) = f else { return XCTFail("\(f)") }
        XCTAssertEqual(s.argv, ["claude", "--resume", "x"])
        XCTAssertEqual(s.state, .needsYou)
        XCTAssertEqual(s.attention?.reason, .permission)
        XCTAssertEqual(s.attention?.detail, "git push")
        XCTAssertEqual(s.attention?.requestId, "42")
        XCTAssertTrue(s.needsYou)
    }

    func testUsageLimitResumeAt() throws {
        let f = try decode(#"{"type":"flow.session","session":{"id":"fs-3","state":"limited","attention":{"reason":"usage_limit","resume_at":"2026-09-07T16:00:00Z"}}}"#)
        guard case let .session(s) = f else { return XCTFail() }
        XCTAssertEqual(s.state, .limited)
        XCTAssertNotNil(s.attention?.resumeDate)
        XCTAssertTrue(s.needsYou)
    }

    func testSessionRemoved() throws {
        guard case let .sessionRemoved(id) = try decode(#"{"type":"flow.session.removed","id":"fs-9"}"#) else { return XCTFail() }
        XCTAssertEqual(id, "fs-9")
    }

    func testOutputDecodesBase64() throws {
        let f = try decode(#"{"type":"flow.output","id":"fs-1","seq":7,"data_b64":"aGVsbG8="}"#)
        guard case let .output(id, seq, data) = f else { return XCTFail() }
        XCTAssertEqual(id, "fs-1")
        XCTAssertEqual(seq, 7)
        XCTAssertEqual(String(data: data, encoding: .utf8), "hello")
    }

    func testScreen() throws {
        guard case let .screen(id, cols, rows, text) = try decode(#"{"type":"flow.screen","id":"fs-1","cols":80,"rows":24,"text":"$ "}"#) else { return XCTFail() }
        XCTAssertEqual([id, text], ["fs-1", "$ "])
        XCTAssertEqual([cols, rows], [80, 24])
    }

    func testAttentionFrameAndNull() throws {
        guard case let .attention(id, a) = try decode(#"{"type":"flow.attention","id":"fs-1","attention":{"reason":"held","pid":48122,"detail":"owned by 48122"}}"#) else { return XCTFail() }
        XCTAssertEqual(id, "fs-1")
        XCTAssertEqual(a?.reason, .held)
        XCTAssertEqual(a?.pid, 48122)
        guard case let .attention(_, cleared) = try decode(#"{"type":"flow.attention","id":"fs-1","attention":null}"#) else { return XCTFail() }
        XCTAssertNil(cleared)
    }

    func testFanout() throws {
        let f = try decode(#"{"type":"flow.fanout","fan_out":{"id":"fo-1","prompt":"p","base_commit":"abc","pearl_id":"th-1","created_at":"x","winner_session_id":null},"candidates":[{"id":"fs-a","fan_out_id":"fo-1"},{"id":"fs-b","fan_out_id":"fo-1"}]}"#)
        guard case let .fanout(fo, cands) = f else { return XCTFail() }
        XCTAssertEqual(fo.baseCommit, "abc")
        XCTAssertEqual(cands.map(\.id), ["fs-a", "fs-b"])
    }

    func testErrorIsObjectNeverBareString() throws {
        guard case let .error(ref, code, message) = try decode(#"{"type":"flow.error","ref":3,"code":"not_found","message":"no such session"}"#) else { return XCTFail() }
        XCTAssertEqual(ref, 3)
        XCTAssertEqual(code, "not_found")
        XCTAssertEqual(message, "no such session")
    }

    func testUnknownTypeAndUnknownEnumsAreNotFatal() throws {
        guard case let .unknown(type) = try decode(#"{"type":"flow.future","whatever":1}"#) else { return XCTFail() }
        XCTAssertEqual(type, "flow.future")
        guard case let .session(s) = try decode(#"{"type":"flow.session","session":{"id":"x","state":"teleporting","attention":{"reason":"vibes"}}}"#) else { return XCTFail() }
        XCTAssertEqual(s.state, .unknown)
        XCTAssertEqual(s.attention?.reason, .unknown)
        XCTAssertFalse(s.needsYou)
    }

    func testMissingTypeIsUnknownAndGarbageThrows() {
        XCTAssertEqual(try decode(#"{"channel":"flow"}"#), .unknown(type: ""))
        XCTAssertThrowsError(try decode("not json"))
    }

    // MARK: client → engine

    private func fields(_ f: ClientFrame) throws -> [String: Any] {
        try XCTUnwrap(JSONSerialization.jsonObject(with: f.encode()) as? [String: Any])
    }

    func testEveryClientFrameCarriesChannelAndType() throws {
        let all: [ClientFrame] = [
            .attach(id: "a", cols: 80, rows: 24), .detach(id: "a"), .input(id: "a", data: Data("x".utf8)), .resize(id: "a", cols: 1, rows: 2),
            .snapshot(id: "a"), .new(NewSession(kind: "claude", pearlId: "th-1", prompt: "go")), .send(id: "a", text: "hi"),
            .approve(id: "a", requestId: "r", decision: .allowSession), .kill(id: "a", resume: true),
            .fanoutNew(prompt: "p", pearlId: nil, candidates: [FanOutCandidate(kind: "claude", model: "opus", label: "A")]),
            .fanoutPick(fanOutId: "fo", winnerSessionId: "a"), .markRead(id: "a"),
        ]
        let types = try all.map { f -> String in
            let o = try fields(f)
            XCTAssertEqual(o["channel"] as? String, "flow")
            return try XCTUnwrap(o["type"] as? String)
        }
        XCTAssertEqual(types, ["flow.attach", "flow.detach", "flow.input", "flow.resize", "flow.snapshot", "flow.new", "flow.send", "flow.approve",
                               "flow.kill", "flow.fanout.new", "flow.fanout.pick", "flow.mark_read"])
    }

    func testClientFrameFieldShapes() throws {
        let input = try fields(.input(id: "a", data: Data("hi".utf8)))
        XCTAssertEqual(input["data_b64"] as? String, "aGk=")
        let approve = try fields(.approve(id: "a", requestId: "r1", decision: .allowSession))
        XCTAssertEqual(approve["request_id"] as? String, "r1")
        XCTAssertEqual(approve["decision"] as? String, "allow_session")
        let new = try fields(.new(NewSession(kind: "claude", pearlId: "th-1")))
        XCTAssertEqual(new["pearl_id"] as? String, "th-1")
        XCTAssertTrue(new["worktree"] is NSNull, "optional fields serialize as JSON null, not missing")
        let fan = try fields(.fanoutNew(prompt: "p", pearlId: nil, candidates: [FanOutCandidate(kind: "codex", model: nil, label: "C")]))
        let cands = try XCTUnwrap(fan["candidates"] as? [[String: Any]])
        XCTAssertEqual(cands.first?["label"] as? String, "C")
        XCTAssertTrue(cands.first?["model"] is NSNull)
        let kill = try fields(.kill(id: "a", resume: false))
        XCTAssertEqual(kill["resume"] as? Bool, false)
        let pick = try fields(.fanoutPick(fanOutId: "fo", winnerSessionId: "w"))
        XCTAssertEqual(pick["winner_session_id"] as? String, "w")
    }
}

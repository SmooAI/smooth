import XCTest
@testable import SmoothFlow

/// Every `flow.*` type from smoothflow-protocol.md decodes; unknown types are ignored.
final class FlowFrameTests: XCTestCase {
    private func decode(_ json: String) throws -> FlowFrame { try FlowFrame.decode(Data(json.utf8)) }

    func testHello() throws {
        let f = try decode(#"{"channel":"flow","type":"flow.hello","daemon":{"version":"1.2","machine_label":"marvin"},"sessions":[{"id":"fs-1","kind":"claude","title":"t","project":"/p","worktree":"/w","branch":"b","pearl_id":"th-1","argv":["claude"],"state":"working","created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z","unread":true}]}"#)
        guard case let .hello(daemon, sessions, harnesses) = f else { return XCTFail("\(f)") }
        XCTAssertEqual(daemon.machineLabel, "marvin")
        XCTAssertEqual(sessions.count, 1)
        XCTAssertEqual(sessions[0].pearlId, "th-1")
        XCTAssertEqual(sessions[0].state, .working)
        XCTAssertTrue(sessions[0].unread)
        XCTAssertEqual(harnesses, [], "a v0 hello without harnesses decodes")
    }

    /// th-0f6126: the harness list rides in the hello and in `flow.harnesses`;
    /// `hidden` is omitted when false on the wire.
    func testHelloWithHarnessesAndHarnessesFrame() throws {
        let f = try decode(#"{"type":"flow.hello","daemon":{"version":"1","machine_label":"m"},"sessions":[],"harnesses":[{"name":"th-code","display_name":"th code","kind":"th-code","installed":true,"binary_path":"/x/th","state_source":"native","order_index":0,"origin":"builtin"},{"name":"aider","installed":false,"reason":"`aider` not found on PATH","state_source":"hooks","hidden":true,"order_index":1,"origin":"user"}]}"#)
        guard case let .hello(_, _, harnesses) = f else { return XCTFail("\(f)") }
        XCTAssertEqual(harnesses.map(\.name), ["th-code", "aider"])
        XCTAssertEqual(harnesses[0].displayName, "th code")
        XCTAssertEqual(harnesses[0].stateSource, "native")
        XCTAssertTrue(harnesses[0].installed)
        XCTAssertFalse(harnesses[0].hidden)
        XCTAssertEqual(harnesses[1].displayName, "aider", "display_name defaults to the name")
        XCTAssertEqual(harnesses[1].pickerLabel, "aider — `aider` not found on PATH")
        XCTAssertTrue(harnesses[1].hidden)
        guard case let .harnesses(list) = try decode(#"{"type":"flow.harnesses","harnesses":[{"name":"codex"}]}"#) else { return XCTFail() }
        XCTAssertEqual(list.map(\.name), ["codex"])
        XCTAssertFalse(list[0].installed, "installed defaults to false")
    }

    /// th-51bf88: the doctor's verdict rides each row once the daemon has
    /// looked; a degraded harness stays launchable but is flagged.
    func testHarnessHealth() throws {
        let f = try decode(#"{"type":"flow.harnesses","harnesses":[{"name":"codex","display_name":"Codex","installed":true,"health":{"verdict":"degraded","reason":"2 of 5 SmoothFlow hooks are not trusted","fix":"codex"}},{"name":"claude","display_name":"Claude Code","installed":true,"health":{"verdict":"works"}},{"name":"aider","installed":false,"reason":"`aider` not found on PATH","health":{"verdict":"not_installed","fix":"uv tool install aider-chat"}},{"name":"pi","installed":true}]}"#)
        guard case let .harnesses(h) = f else { return XCTFail("\(f)") }
        XCTAssertTrue(h[0].isDegraded)
        XCTAssertEqual(h[0].pickerLabel, "Codex — needs setup")
        XCTAssertEqual(h[0].health?.fix, "codex")
        XCTAssertFalse(h[1].isDegraded)
        XCTAssertEqual(h[1].pickerLabel, "Claude Code")
        XCTAssertFalse(h[2].isDegraded, "not installed is its own state, not degraded")
        XCTAssertEqual(h[2].pickerLabel, "aider — `aider` not found on PATH")
        XCTAssertEqual(h[2].health?.fix, "uv tool install aider-chat")
        XCTAssertNil(h[3].health, "no verdict until the daemon's first pass")
        XCTAssertFalse(h[3].isDegraded)
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
            .close(id: "a", closePearl: true, removeWorktree: true, force: false),
        ]
        let types = try all.map { f -> String in
            let o = try fields(f)
            XCTAssertEqual(o["channel"] as? String, "flow")
            return try XCTUnwrap(o["type"] as? String)
        }
        XCTAssertEqual(types, ["flow.attach", "flow.detach", "flow.input", "flow.resize", "flow.snapshot", "flow.new", "flow.send", "flow.approve",
                               "flow.kill", "flow.fanout.new", "flow.fanout.pick", "flow.mark_read", "flow.close"])
    }

    /// th-883ce9: `flow.close {id, close_pearl, remove_worktree, force}` — the
    /// engine's names, booleans always present (its serde defaults are off).
    func testCloseFrameShape() throws {
        let o = try fields(.close(id: "fs-1", closePearl: true, removeWorktree: false, force: true))
        XCTAssertEqual(o["id"] as? String, "fs-1")
        XCTAssertEqual(o["close_pearl"] as? Bool, true)
        XCTAssertEqual(o["remove_worktree"] as? Bool, false)
        XCTAssertEqual(o["force"] as? Bool, true)
        XCTAssertNil(o["seq"], "no seq unless asked for")
        XCTAssertEqual(Set(o.keys), ["channel", "type", "id", "close_pearl", "remove_worktree", "force"])
    }

    /// A client `seq` rides at the top level (what `client_seq` reads) and is
    /// echoed by the engine as `flow.error.ref`; frames without one are unchanged.
    func testSeqIsOptionalAndTopLevel() throws {
        let tagged = try XCTUnwrap(JSONSerialization.jsonObject(with: ClientFrame.close(id: "a", closePearl: false, removeWorktree: true, force: false).encode(seq: 42)) as? [String: Any])
        XCTAssertEqual(tagged["seq"] as? Int, 42)
        XCTAssertEqual(tagged["type"] as? String, "flow.close")
        XCTAssertTrue(ClientFrame.markRead(id: "a").encodeText(seq: 7).contains("\"seq\":7"))
        XCTAssertEqual(ClientFrame.markRead(id: "a").encodeText(), ClientFrame.markRead(id: "a").encodeText(seq: nil))
        XCTAssertFalse(ClientFrame.markRead(id: "a").encodeText().contains("seq"))
        guard case let .error(ref, code, message) = try decode(#"{"type":"flow.error","ref":42,"code":"refused","message":"branch not merged"}"#) else { return XCTFail() }
        XCTAssertEqual(ref, 42)
        XCTAssertEqual(code, "refused")
        XCTAssertEqual(message, "branch not merged")
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

/// Frames the engine follow-up adds (flow.event, flow.handoff, client hello)
/// and the wire facts the real engine pinned: text frames, token-gated routes.
final class FlowFrameIntegrationTests: XCTestCase {
    private func decode(_ json: String) throws -> FlowFrame { try FlowFrame.decode(Data(json.utf8)) }

    func testEventDecodes() throws {
        let f = try decode(#"{"channel":"flow","type":"flow.event","id":"fs-1","event_id":"ev-9","at":"2026-09-08T12:00:00Z","kind":"PreToolUse","text":"Bash: git status"}"#)
        guard case let .event(e) = f else { return XCTFail("\(f)") }
        XCTAssertEqual(e.sessionId, "fs-1")
        XCTAssertEqual(e.eventId, "ev-9")
        XCTAssertEqual(e.kind, "PreToolUse")
        XCTAssertEqual(e.text, "Bash: git status")
        XCTAssertFalse(e.needsYou)
        guard case let .event(p) = try decode(#"{"type":"flow.event","id":"fs-1","event_id":7,"kind":"permission","text":{"command":"rm -rf x"}}"#) else { return XCTFail() }
        XCTAssertEqual(p.eventId, "7", "numeric ids stringify")
        XCTAssertEqual(p.text, "rm -rf x", "object text renders like attention detail")
        XCTAssertTrue(p.needsYou)
        guard case let .event(q) = try decode(#"{"type":"flow.event","id":"fs-1","kind":"Stop","text":"done"}"#) else { return XCTFail() }
        XCTAssertFalse(q.eventId.isEmpty, "a missing event_id gets a synthetic one so the row is still Identifiable")
    }

    func testHandoffPushDecodesLikeTheGet() throws {
        let f = try decode(#"{"type":"flow.handoff","id":"fs-1","pearl":{"id":"th-1","title":"T","status":"in_progress","priority":2,"labels":["x"]},"handoff":{"worktree":"/w","branch":"b","head":"abc","dirty":["a.rs"],"agent_session_id":"u","next":"push"},"checkpoints":[{"at":"2026-09-08T00:00:00Z","note":"n","auto":false}],"blocks":["th-2"],"pr":{"number":5,"url":"https://x/5","ci":"green"}}"#)
        guard case let .handoff(id, h) = f else { return XCTFail("\(f)") }
        XCTAssertEqual(id, "fs-1")
        XCTAssertEqual(h.pearl?.title, "T")
        XCTAssertEqual(h.handoff?.dirty, ["a.rs"])
        XCTAssertEqual(h.checkpoints?.first?.note, "n")
        XCTAssertEqual(h.pr?.number, 5)
        // The real engine degrades `pearl` to {id, text} when th lacks `show --json`.
        guard case let .handoff(_, thin) = try decode(#"{"type":"flow.handoff","id":"fs-1","pearl":{"id":"th-1","text":"raw"},"handoff":{"worktree":"/w"},"checkpoints":[],"blocks":[],"pr":null}"#) else { return XCTFail() }
        XCTAssertEqual(thin.pearl?.id, "th-1")
        XCTAssertNil(thin.pr)
    }

    func testErrorRefMayBeAnyJson() throws {
        guard case let .error(ref, code, _) = try decode(#"{"type":"flow.error","ref":"abc","code":"bad_frame","message":"m"}"#) else { return XCTFail() }
        XCTAssertNil(ref)
        XCTAssertEqual(code, "bad_frame")
        guard case let .error(none, _, _) = try decode(#"{"type":"flow.error","ref":null,"code":"x","message":"m"}"#) else { return XCTFail() }
        XCTAssertNil(none)
    }

    func testClientHelloAndTextEncoding() throws {
        let hello = ClientFrame.hello(client: "smoothflow", version: "0.1")
        let o = try XCTUnwrap(JSONSerialization.jsonObject(with: hello.encode()) as? [String: Any])
        XCTAssertEqual(o["type"] as? String, "flow.hello")
        XCTAssertEqual(o["channel"] as? String, "flow")
        XCTAssertEqual(o["client"] as? String, "smoothflow")
        let text = ClientFrame.input(id: "a", data: Data("hi".utf8)).encodeText()
        XCTAssertTrue(text.hasPrefix("{") && text.contains(#""data_b64":"aGk=""#), "one JSON object per TEXT message: \(text)")
    }

    func testEngineSessionRowDecodes() throws {
        // A row exactly as the real engine serializes it (RFC3339 with nanos, argv array, pid_start skipped).
        let json = #"{"channel":"flow","type":"flow.session","session":{"id":"fs-7b0a1c2d","kind":"shell","title":"zsh","project":"/Users/u/dev/x","worktree":"/Users/u/dev/x","branch":"main","pearl_id":null,"agent_session_id":null,"argv":["zsh","-l"],"tmux_session":"fs-7b0a1c2d","pid":4242,"state":"starting","attention":null,"fan_out_id":null,"created_at":"2026-09-08T16:01:02.123456789Z","updated_at":"2026-09-08T16:01:02.123456789Z","ended_at":null,"exit_code":null,"unread":false}}"#
        guard case let .session(s) = try decode(json) else { return XCTFail() }
        XCTAssertEqual(s.tmuxSession, "fs-7b0a1c2d")
        XCTAssertEqual(s.pid, 4242)
        XCTAssertEqual(s.state, .starting)
        XCTAssertEqual(s.argv, ["zsh", "-l"])
        XCTAssertNotNil(ISO8601DateFormatter.flexible.date(from: s.createdAt), "nanosecond timestamps parse")
    }

    /// th-145e6b: the directory picker's rows and its pure helpers.
    func testRepoListAndDirectoryPicking() throws {
        let json = #"{"repos":[{"path":"/Users/me/dev/smooai/smooth-th-1","name":"smooth-th-1","branch":"th-1","main":"/Users/me/dev/smooai/smooth","touched":5},{"path":"/Users/me/dev/refs/cmux","name":"cmux","touched":1}],"scanning":true,"indexed":false}"#
        let list = try JSONDecoder().decode(RepoList.self, from: Data(json.utf8))
        XCTAssertEqual(list.repos.map(\.name), ["smooth-th-1", "cmux"])
        XCTAssertEqual(list.repos[0].main, "/Users/me/dev/smooai/smooth")
        XCTAssertNil(list.repos[1].branch)
        XCTAssertTrue(list.scanning)
        XCTAssertEqual(DirectoryPicking.abbreviate("/Users/me/dev/x", home: "/Users/me"), "~/dev/x")
        XCTAssertEqual(DirectoryPicking.abbreviate("/Users/me", home: "/Users/me"), "~")
        XCTAssertEqual(DirectoryPicking.abbreviate("/Users/meta/x", home: "/Users/me"), "/Users/meta/x", "a prefix that is not a path boundary")
        XCTAssertEqual(DirectoryPicking.expandedPath("~/dev", home: "/Users/me"), "/Users/me/dev")
        XCTAssertEqual(DirectoryPicking.expandedPath(" /tmp/x ", home: "/Users/me"), "/tmp/x")
        XCTAssertNil(DirectoryPicking.expandedPath("smooth", home: "/Users/me"), "a word is a search, not a path")
        XCTAssertEqual(DirectoryPicking.moved(0, by: -1, count: 3), 0)
        XCTAssertEqual(DirectoryPicking.moved(2, by: 1, count: 3), 2)
        XCTAssertEqual(DirectoryPicking.moved(0, by: 1, count: 0), 0)
    }
}

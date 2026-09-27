@testable import SmoothFlow
import XCTest

/// Replays the Client Spec conformance vectors (`spec/vectors/*.json`, written
/// by `crates/smooth-flow-client`) against this app's own implementation
/// (th-3e6020). A failure here means the Mac app disagrees with the spec — fix
/// the app, or, if the spec changed on purpose, re-bless the vectors in Rust.
final class ConformanceVectorTests: XCTestCase {
    private func cases(_ file: String) throws -> [[String: Any]] {
        // Tests/ → apps/smoothflow → apps → repo root.
        let root = URL(fileURLWithPath: #filePath).deletingLastPathComponent().deletingLastPathComponent()
            .deletingLastPathComponent().deletingLastPathComponent()
        let data = try Data(contentsOf: root.appendingPathComponent("spec/vectors/\(file)"))
        let json = try XCTUnwrap(JSONSerialization.jsonObject(with: data) as? [String: Any])
        return try XCTUnwrap(json["cases"] as? [[String: Any]])
    }

    private func session(_ v: Any?) throws -> Session? {
        guard let dict = v as? [String: Any] else { return nil }
        let data = try JSONSerialization.data(withJSONObject: dict)
        return try JSONDecoder().decode(Session.self, from: data)
    }

    func testClose() throws {
        for c in try cases("close.json") {
            let name = c["name"] as? String ?? "?"
            let input = try XCTUnwrap(c["input"] as? [String: Any])
            let expected = try XCTUnwrap(c["expected"] as? [String: Any])
            let scope = PaneCloseScope.of(panes: input["panes"] as? Int ?? 0, tabs: input["tabs"] as? Int ?? 0)
            XCTAssertEqual(scope.rawValue, expected["scope"] as? String, name)
            let d = PaneClose.decide(session: try session(input["session"]), harnessLabel: "", scope: scope,
                                     shownElsewhere: input["shown_elsewhere"] as? Bool ?? false,
                                     confirmEnabled: input["confirm_enabled"] as? Bool ?? true)
            if let prompt = expected["prompt"] as? [String: Any] {
                let p = try XCTUnwrap(d.prompt, name)
                XCTAssertEqual(p.title, prompt["title"] as? String, name)
                XCTAssertEqual(p.closeTitle, prompt["close_title"] as? String, name)
                XCTAssertEqual(p.killTitle, prompt["kill_title"] as? String, name)
            } else {
                XCTAssertNil(d.prompt, name)
            }
        }
    }

    func testTitle() throws {
        for c in try cases("title.json") {
            let input = try XCTUnwrap(c["input"] as? [String: Any])
            let s = try XCTUnwrap(try session(input["session"]))
            let want = (c["expected"] as? [String: Any])?["title"] as? String
            XCTAssertEqual(s.tabTitle(home: input["home"] as? String ?? ""), want, c["name"] as? String ?? "?")
        }
    }

    func testGate() throws {
        for c in try cases("gate.json") {
            let name = c["name"] as? String ?? "?"
            let input = try XCTUnwrap(c["input"] as? [String: Any])
            let kind = input["kind"] as? String ?? ""
            let s = Session(id: "fs-1", kind: kind, branch: input["branch"] as? String)
            var packet: Handoff.Packet?
            if let p = input["packet"] as? [String: Any] {
                packet = Handoff.Packet(worktree: nil, branch: p["branch"] as? String, head: p["head"] as? String, dirty: nil, agentSessionId: nil, next: nil)
            }
            let harnesses = (input["state_source"] as? String).map { [HarnessInfo(name: kind, stateSource: $0)] } ?? []
            let g = CenterTabGate.of(session: s, packet: packet, harnesses: harnesses)
            let want = try XCTUnwrap((c["expected"] as? [String: Any])?["gate"] as? [String: Any])
            XCTAssertEqual(g.diff, want["diff"] as? Bool, name)
            XCTAssertEqual(g.pr, want["pr"] as? Bool, name)
            let depth: String = switch g.activity {
            case .full: "full"
            case .thin: "thin"
            case .none: "none"
            }
            XCTAssertEqual(depth, want["activity"] as? String, name)
        }
    }

    func testDirectory() throws {
        for c in try cases("directory.json") {
            let input = try XCTUnwrap(c["input"] as? [String: Any])
            let home = input["home"] as? String ?? ""
            switch input["fn"] as? String {
            case "abbreviate":
                XCTAssertEqual(DirectoryPicking.abbreviate(input["path"] as? String ?? "", home: home), c["expected"] as? String)
            case "expanded_path":
                XCTAssertEqual(DirectoryPicking.expandedPath(input["typed"] as? String ?? "", home: home), c["expected"] as? String)
            case "moved":
                XCTAssertEqual(DirectoryPicking.moved(input["index"] as? Int ?? 0, by: input["delta"] as? Int ?? 0, count: input["count"] as? Int ?? 0),
                               c["expected"] as? Int)
            default:
                XCTFail("unknown fn in directory.json")
            }
        }
    }

    func testFleet() throws {
        for c in try cases("fleet.json") {
            let input = try XCTUnwrap(c["input"] as? [String: Any])
            let sessions = try (input["sessions"] as? [Any] ?? []).compactMap { try session($0) }
            let expected = try XCTUnwrap(c["expected"] as? [String: Any])
            let groups = FlowStore.grouped(sessions).map { [$0.project] + $0.sessions.map(\.id) }
            let want = (expected["groups"] as? [[String: Any]] ?? []).map { g in [g["project"] as? String ?? ""] + (g["sessions"] as? [String] ?? []) }
            XCTAssertEqual(groups, want)
            let n = FlowStore.counts(sessions)
            let wc = try XCTUnwrap(expected["counts"] as? [String: Int])
            XCTAssertEqual([n.working, n.needsYou, n.done, n.idle], [wc["working"], wc["needs_you"], wc["done"], wc["idle"]].map { $0 ?? -1 })
        }
    }

    /// Pane scripts. The vectors' geometry is top-down; AppKit's is bottom-up,
    /// so vertical splits mirror — the pane a direction reaches is the same.
    func testPane() throws {
        for c in try cases("pane.json") {
            let name = c["name"] as? String ?? "?"
            let input = try XCTUnwrap(c["input"] as? [String: Any])
            let ops = try XCTUnwrap(input["ops"] as? [[String: Any]])
            let steps = try XCTUnwrap((c["expected"] as? [String: Any])?["steps"] as? [[String: Any]])
            var tab = SurfaceTab(id: 1, pane: PaneID(value: 1))
            tab.sessions[PaneID(value: 1)] = "fs-1"
            let bounds = CGRect(x: 0, y: 0, width: 100, height: 100)
            for (i, op) in ops.enumerated() {
                let dir = (op["direction"] as? String).flatMap(SplitDirection.init(rawValue:))
                var returned: Bool?
                switch op["op"] as? String {
                case "split": if let dir, let n = op["new"] as? Int { tab.split(dir, newID: PaneID(value: n)) }
                case "close": returned = tab.closeFocused()
                case "focus": if let dir { tab.focus(dir, in: bounds) }
                case "zoom": tab.toggleZoom()
                case "equalize": tab.equalize()
                default: XCTFail("unknown op \(op)")
                }
                let want = steps[i]
                XCTAssertEqual(tab.panes.map(\.value), want["panes"] as? [Int], "\(name) step \(i)")
                XCTAssertEqual(tab.focused.value, want["focused"] as? Int, "\(name) step \(i)")
                XCTAssertEqual(tab.zoomed?.value, want["zoomed"] as? Int, "\(name) step \(i)")
                if let r = returned { XCTAssertEqual(r, want["returned"] as? Bool, "\(name) step \(i)") }
            }
        }
    }
}

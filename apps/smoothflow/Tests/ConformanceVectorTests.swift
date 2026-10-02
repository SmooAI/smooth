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

    /// th-26f5b9: the Diff viewer's rules (spec §14).
    func testDiff() throws {
        func pos(_ v: Any?) -> [Int]? { v as? [Int] }
        for c in try cases("diff.json") {
            let name = c["name"] as? String ?? "?"
            let input = try XCTUnwrap(c["input"] as? [String: Any])
            let expected = c["expected"]
            switch input["fn"] as? String {
            case "side_by_side":
                let kinds = try XCTUnwrap(input["kinds"] as? [String]).compactMap(DiffLineKind.init(rawValue:))
                let rows = DiffRules.sideBySide(kinds).map { [$0.left ?? -1, $0.right ?? -1] }
                let want = try XCTUnwrap((expected as? [String: Any])?["rows"] as? [[String: Any]]).map { [$0["left"] as? Int ?? -1, $0["right"] as? Int ?? -1] }
                XCTAssertEqual(rows, want, name)
            case "tree":
                let paths = try XCTUnwrap(input["paths"] as? [String])
                let exp = try XCTUnwrap(expected as? [String: Any])
                let rows = DiffRules.tree(paths).map { "\($0.kind)|\($0.name)|\($0.path)|\($0.depth)|\($0.file ?? -1)" }
                let want = try XCTUnwrap(exp["rows"] as? [[String: Any]]).map {
                    "\($0["kind"] as? String ?? "")|\($0["name"] as? String ?? "")|\($0["path"] as? String ?? "")|\($0["depth"] as? Int ?? -1)|\($0["file"] as? Int ?? -1)"
                }
                XCTAssertEqual(rows, want, name)
                XCTAssertEqual(DiffRules.fileOrder(paths), exp["file_order"] as? [Int], name)
            case "display":
                let data = try JSONSerialization.data(withJSONObject: try XCTUnwrap(input["file"]))
                let file = try JSONDecoder().decode(DiffFile.self, from: data)
                let exp = try XCTUnwrap(expected as? [String: Any])
                let d = DiffRules.display(file, viewed: input["viewed"] as? Bool ?? false)
                let want = try XCTUnwrap(exp["display"] as? [String: Any])
                XCTAssertEqual(d.collapsed, want["collapsed"] as? Bool, name)
                XCTAssertEqual(d.reason, want["reason"] as? String, name)
                XCTAssertEqual(d.fetch, want["fetch"] as? Bool, name)
                XCTAssertEqual(DiffRules.viewedKey(file), exp["viewed_key"] as? String, name)
            case "next_hunk":
                let counts = try XCTUnwrap(input["hunks"] as? [Int])
                let collapsed = try XCTUnwrap(input["collapsed"] as? [Bool])
                let order = try XCTUnwrap(input["order"] as? [Int])
                var at: DiffRules.Pos?
                for step in try XCTUnwrap((expected as? [String: Any])?["steps"] as? [[String: Any]]) {
                    let forward = step["forward"] as? Bool ?? true
                    at = DiffRules.nextHunk(hunkCounts: counts, order: order, collapsed: collapsed, at: at, forward: forward) ?? at
                    XCTAssertEqual(at.map { [$0.file, $0.hunk] }, pos(step["at"]), name)
                }
            case "next_file":
                let order = try XCTUnwrap(input["order"] as? [Int])
                var at: Int?
                for step in try XCTUnwrap((expected as? [String: Any])?["steps"] as? [[String: Any]]) {
                    at = DiffRules.nextFile(order: order, at: at, forward: step["forward"] as? Bool ?? true) ?? at
                    XCTAssertEqual(at, step["at"] as? Int, name)
                }
            case "default_base":
                XCTAssertEqual(DiffRules.defaultBase(kind: input["kind"] as? String ?? "").rawValue, expected as? String, name)
            case "base_label":
                let base = try XCTUnwrap(DiffBase(rawValue: input["base"] as? String ?? ""))
                let ref = (input["from_label"] as? String).flatMap(DiffRules.branchRef(fromLabel:))
                XCTAssertEqual(DiffRules.baseLabel(base, branchRef: ref), expected as? String, name)
            case "keys":
                let want = try XCTUnwrap(expected as? [[String: Any]])
                for k in want {
                    let key = try XCTUnwrap(k["key"] as? String)
                    let action = Keymap.default.diffAction(for: KeyChord(key))
                    XCTAssertEqual(action?.diffSpecName, k["action"] as? String, "key \(key)")
                }
                XCTAssertEqual(FlowAction.allCases.filter(\.isViewScoped).count, want.count, "every spec key has an action and no more")
            default:
                XCTFail("unknown fn in \(name)")
            }
        }
    }
}

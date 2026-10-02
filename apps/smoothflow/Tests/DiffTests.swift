import AppKit
import XCTest
@testable import SmoothFlow

/// th-26f5b9: the Diff tab's wire decoding, layout and text styling.
final class DiffTests: XCTestCase {
    private let wire = #"""
    {"channel":"flow","type":"flow.diff","id":"fs-1","base":"uncommitted","diff":{
      "base":"uncommitted","from":{"ref":"aaa","label":"HEAD"},"to":{"ref":"bbb","label":"worktree"},
      "files":[
        {"path":"src/lib.rs","status":"modified","language":"Rust","added":1,"deleted":1,"hunks":[
          {"id":"h1","old_start":1,"old_lines":2,"new_start":1,"new_lines":2,"section":"fn main() {","lines":[
            {"kind":"ctx","old":1,"new":1,"text":"let a = 1;","syntax":[[0,3,0],[8,9,3]]},
            {"kind":"del","old":2,"text":"let café = 2;","words":[[11,12]]},
            {"kind":"add","new":2,"text":"let café = 3;","words":[[11,12]]}
          ],"staged":true}]},
        {"path":"Cargo.lock","status":"modified","added":40,"deleted":2,"noise":"lockfile","collapsed_by_default":true,"hunks_omitted":"collapsed"},
        {"path":"logo.png","status":"added","binary":true,"added":0,"deleted":0}
      ],
      "added":41,"deleted":3,"legend":["keyword","string","comment","number"]}}
    """#

    private func payload() throws -> DiffPayload {
        guard case let .diff(id, base, path, d) = try FlowFrame.decode(Data(wire.utf8)) else { throw XCTSkip("not a diff frame") }
        XCTAssertEqual(id, "fs-1")
        XCTAssertEqual(base, .uncommitted)
        XCTAssertNil(path)
        return d
    }

    func testDecodesTheWire() throws {
        let d = try payload()
        XCTAssertEqual(d.files.count, 3)
        XCTAssertEqual(d.added, 41)
        let h = d.files[0].hunks[0]
        XCTAssertTrue(h.staged)
        XCTAssertEqual(h.header, "@@ -1,2 +1,2 @@ fn main() {")
        XCTAssertEqual(h.lines[1].kind, .del)
        XCTAssertEqual(h.lines[1].words, [[11, 12]])
        XCTAssertEqual(d.files[1].hunksOmitted, "collapsed")
        XCTAssertTrue(d.files[2].binary)
        XCTAssertEqual(d.files[2].badge, "A")
        XCTAssertEqual(try FlowFrame.decode(Data(#"{"type":"flow.diff.changed","id":"fs-1"}"#.utf8)), .diffChanged(id: "fs-1"))
        guard case let .diffResult(r) = try FlowFrame.decode(Data(#"{"type":"flow.diff.result","id":"fs-1","action":"revert","hunk_id":"h1","file":"a.rs"}"#.utf8))
        else { return XCTFail() }
        XCTAssertEqual(r.hunkId, "h1")
    }

    func testClientFramesEncode() throws {
        let c = DiffComment(file: "a.rs", hunkId: "h1", range: 3...5, text: "why?")
        let obj = try XCTUnwrap(JSONSerialization.jsonObject(with: ClientFrame.diffReview(id: "fs-1", base: .turn, comments: [c]).encode()) as? [String: Any])
        XCTAssertEqual(obj["type"] as? String, "flow.diff.review")
        XCTAssertEqual(obj["base"] as? String, "turn")
        let first = try XCTUnwrap((obj["comments"] as? [[String: Any]])?.first)
        XCTAssertEqual(first["line_range"] as? [Int], [3, 5])
        XCTAssertEqual(first["hunk_id"] as? String, "h1")
        let diff = try XCTUnwrap(JSONSerialization.jsonObject(with: ClientFrame.diff(id: "fs-1", base: .branch, path: nil).encode()) as? [String: Any])
        XCTAssertEqual(diff["type"] as? String, "flow.diff")
        XCTAssertTrue(diff["path"] is NSNull)
        let stage = try XCTUnwrap(JSONSerialization.jsonObject(with: ClientFrame.diffStage(id: "fs-1", hunkId: "h1").encode()) as? [String: Any])
        XCTAssertEqual(stage["base"] as? String, "uncommitted")
    }

    func testLayoutUnifiedSplitCollapsedAndComments() throws {
        let d = try payload()
        let collapsed = d.files.map { DiffRules.display($0, viewed: false).collapsed }
        XCTAssertEqual(collapsed, [false, true, true])
        // Tree order: src/ first (a directory), then Cargo.lock, logo.png.
        let unified = DiffLayout.rows(d, split: false, collapsed: collapsed, viewed: [false, false, false], comments: [])
        XCTAssertEqual(unified, [
            .fileHeader(file: 0), .hunkHeader(file: 0, hunk: 0),
            .line(file: 0, hunk: 0, line: 0), .line(file: 0, hunk: 0, line: 1), .line(file: 0, hunk: 0, line: 2),
            .fileHeader(file: 1), .notice(file: 1, text: "Lockfile · +40 −2 · collapsed", fetch: true),
            .fileHeader(file: 2), .notice(file: 2, text: "Binary file", fetch: false),
        ])
        let split = DiffLayout.rows(d, split: true, collapsed: collapsed, viewed: [false, false, false], comments: [])
        XCTAssertTrue(split.contains(.pair(file: 0, hunk: 0, left: 1, right: 2)), "\(split)")
        XCTAssertTrue(split.contains(.pair(file: 0, hunk: 0, left: 0, right: 0)))

        let comment = DiffComment(file: "src/lib.rs", hunkId: "h1", range: 2...2, text: "x")
        let fileComment = DiffComment(file: "src/lib.rs", hunkId: nil, range: nil, text: "general")
        let withComments = DiffLayout.rows(d, split: false, collapsed: collapsed, viewed: [false, false, false], comments: [comment, fileComment])
        let at = try XCTUnwrap(withComments.firstIndex(of: .comment(file: 0, index: 0)))
        XCTAssertEqual(withComments[at - 1], .line(file: 0, hunk: 0, line: 2), "a comment sits under the last line of its range")
        XCTAssertEqual(withComments[1], .comment(file: 0, index: 1), "a file comment sits under the header")

        let viewed = DiffLayout.rows(d, split: false, collapsed: [true, true, true], viewed: [true, false, false], comments: [])
        XCTAssertEqual(viewed[1], .notice(file: 0, text: "Viewed", fetch: false))

        var note = DiffPayload(base: .turn, note: "No turn recorded yet")
        note.truncated = true
        note.filesOmitted = 2
        let banners = DiffLayout.rows(note, split: false, collapsed: [], viewed: [], comments: [])
        XCTAssertEqual(banners.count, 2)
    }

    func testStylerMapsScalarSpansToUTF16() throws {
        let d = try payload()
        let font = NSFont.monospacedSystemFont(ofSize: 12, weight: .regular)
        let ctx = DiffStyler.line(d.files[0].hunks[0].lines[0], legend: d.legend, font: font)
        XCTAssertEqual(ctx.attribute(.foregroundColor, at: 0, effectiveRange: nil) as? NSColor, DiffPalette.mauve, "keyword")
        XCTAssertEqual(ctx.attribute(.foregroundColor, at: 8, effectiveRange: nil) as? NSColor, DiffPalette.peach, "number")
        let add = DiffStyler.line(d.files[0].hunks[0].lines[2], legend: d.legend, font: font)
        XCTAssertEqual(add.attribute(.backgroundColor, at: 11, effectiveRange: nil) as? NSColor, DiffPalette.addWord)
        XCTAssertNil(add.attribute(.backgroundColor, at: 4, effectiveRange: nil))

        // An astral character is two UTF-16 units but one scalar.
        let offsets = DiffText.utf16Offsets("a😀b")
        XCTAssertEqual(offsets, [0, 1, 3, 4])
        XCTAssertEqual(DiffText.nsRange(offsets, 2, 3), NSRange(location: 3, length: 1))
        XCTAssertNil(DiffText.nsRange(offsets, 3, 3))
        XCTAssertEqual(DiffText.nsRange(offsets, 1, 99), NSRange(location: 1, length: 3), "clamped")
    }

    func testDiffActionsAreNotMenuItems() {
        for a in FlowAction.allCases where a.isViewScoped {
            XCTAssertNotNil(a.diffSpecName, a.rawValue)
            XCTAssertFalse(a.defaultChord?.hasModifier ?? true, a.rawValue)
        }
    }
}

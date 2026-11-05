import AppKit
@testable import SmoothFlow
import XCTest

/// SMOODEV-4266: the palette lists the keymap's registry and the fleet.
@MainActor
final class CommandPaletteTests: XCTestCase {
    private let sample = [
        PaletteItem(target: .session("a"), title: "th-1 fix the login page", detail: "smooai · working"),
        PaletteItem(target: .action(.splitRight), title: "Split Right", detail: "Splits", chord: "⇧⌘→"),
        PaletteItem(target: .action(.splitDown), title: "Split Down", detail: "Splits"),
        PaletteItem(target: .action(.settings), title: "Settings", detail: "View"),
    ]

    func testEveryActionIsListedOnceAndNothingUnreachable() {
        let actions = CommandPalette.actions { _ in nil }
        XCTAssertEqual(Set(actions.map(\.id)).count, actions.count)
        for a in FlowAction.allCases {
            let listed = actions.contains { $0.target == .action(a) }
            let hidden = a.isViewScoped || a.isAlias || FlowAction.focusSessionIndex(a) != nil || a == .commandPalette
            XCTAssertEqual(listed, !hidden, a.rawValue)
        }
    }

    func testChordsComeFromTheKeymapSoARebindShows() {
        let chord = KeyChord("e", command: true, option: true)
        let items = CommandPalette.actions { $0 == .inbox ? chord : nil }
        XCTAssertEqual(items.first { $0.target == .action(.inbox) }?.chord, chord.display)
        XCTAssertNil(items.first { $0.target == .action(.kill) }?.chord)
    }

    func testTitlesDropTheMenuEllipsis() {
        let items = CommandPalette.actions { _ in nil }
        XCTAssertEqual(items.first { $0.target == .action(.settings) }?.title, "Settings")
    }

    func testSessionsShowLabelProjectAndState() {
        let s = Session(id: "fs-1", title: "fix", project: "smooai", state: .working)
        let item = CommandPalette.sessions([s])[0]
        XCTAssertEqual(item.target, .session("fs-1"))
        XCTAssertEqual(item.title, "fix")
        XCTAssertEqual(item.detail, "smooai · working")
    }

    func testEmptyQueryKeepsTheGivenOrder() {
        XCTAssertEqual(CommandPalette.filter(sample, query: "  "), sample)
    }

    func testSubsequenceMatchesAndMissesDoNot() {
        XCTAssertEqual(CommandPalette.filter(sample, query: "sr").first?.target, .action(.splitRight))
        XCTAssertTrue(CommandPalette.filter(sample, query: "zzz").isEmpty)
    }

    func testEveryTokenMustMatch() {
        XCTAssertEqual(CommandPalette.filter(sample, query: "split down").map(\.target), [.action(.splitDown)])
        XCTAssertTrue(CommandPalette.filter(sample, query: "split zzz").isEmpty)
    }

    func testDetailIsSearchable() {
        XCTAssertEqual(CommandPalette.filter(sample, query: "smooai").first?.target, .session("a"))
    }

    func testPrefixOutranksScattered() throws {
        let scattered = PaletteItem(target: .action(.kill), title: "xsxexttxixnxgxs", detail: "")
        XCTAssertEqual(CommandPalette.filter([scattered, sample[3]], query: "set").first?.target, .action(.settings))
    }

    func testScoreEdges() {
        XCTAssertEqual(CommandPalette.score("", in: "x"), 0)
        XCTAssertNil(CommandPalette.score("abc", in: "ab"))
        XCTAssertNil(CommandPalette.score("ba", in: "ab"), "order matters")
        XCTAssertNotNil(CommandPalette.score("AB", in: "ab"), "case does not")
    }

    func testMoveWrapsAndQueryResetsSelection() {
        let m = CommandPaletteModel(items: sample)
        m.move(by: -1)
        XCTAssertEqual(m.current, sample[3])
        m.move(by: 1)
        XCTAssertEqual(m.current, sample[0])
        m.move(by: 1)
        m.setQuery("settings")
        XCTAssertEqual(m.selected, 0)
        XCTAssertEqual(m.current?.target, .action(.settings))
        m.setQuery("zzz")
        XCTAssertNil(m.current)
        m.move(by: 1)
        XCTAssertNil(m.current, "moving through nothing stays safe")
    }

    func testTheWindowFiltersMovesRunsAndCloses() throws {
        let c = CommandPaletteController()
        var ran: [PaletteItem.Target] = []
        c.show(over: nil, items: sample) { ran.append($0) }
        XCTAssertTrue(c.isShown)
        let field = try XCTUnwrap(c.field)
        let editor = NSTextView()

        XCTAssertTrue(c.control(field, textView: editor, doCommandBy: #selector(NSResponder.moveDown(_:))))
        XCTAssertEqual(c.model?.current, sample[1])
        XCTAssertFalse(c.control(field, textView: editor, doCommandBy: #selector(NSResponder.moveLeft(_:))), "ordinary editing stays the field's")

        XCTAssertTrue(c.control(field, textView: editor, doCommandBy: #selector(NSResponder.insertNewline(_:))))
        XCTAssertEqual(ran, [.action(.splitRight)])
        XCTAssertFalse(c.isShown, "a choice closes it")

        c.show(over: nil, items: sample) { ran.append($0) }
        XCTAssertTrue(c.control(try XCTUnwrap(c.field), textView: editor, doCommandBy: #selector(NSResponder.cancelOperation(_:))))
        XCTAssertFalse(c.isShown)
        XCTAssertEqual(ran.count, 1, "Esc runs nothing")
    }

    func testToggleClosesAnOpenPalette() {
        let c = CommandPaletteController()
        c.toggle(over: nil, items: sample) { _ in }
        XCTAssertTrue(c.isShown)
        c.toggle(over: nil, items: sample) { _ in }
        XCTAssertFalse(c.isShown)
    }

    func testItSitsNearTheTopOfTheWindow() {
        let origin = CommandPaletteController.origin(in: NSRect(x: 100, y: 100, width: 1000, height: 800), size: NSSize(width: 560, height: 380))
        XCTAssertEqual(origin.x, 320)
        XCTAssertEqual(origin.y, 900 - 380 - 72)
    }
}

final class SidebarNoteTests: XCTestCase {
    func testTheNoteIsTheFirstLineOfWhatTheSessionIsAskingFor() {
        var s = Session(id: "a")
        XCTAssertNil(s.sidebarNote)
        s.attention = Attention(reason: .permission, detail: "  Bash: git push  \nsecond line")
        XCTAssertEqual(s.sidebarNote, "Bash: git push")
        s.attention = Attention(reason: .question, detail: "   ")
        XCTAssertNil(s.sidebarNote)
        s.attention = nil
        XCTAssertNil(s.sidebarNote, "gone once it stops asking")
    }
}

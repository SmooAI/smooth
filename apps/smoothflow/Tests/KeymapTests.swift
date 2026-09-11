import AppKit
@testable import SmoothFlow
import XCTest

final class KeyChordTests: XCTestCase {
    func testParsesModifiersInAnyOrderAndCase() {
        let expected = KeyChord("d", command: true, shift: true)
        for text in ["cmd+shift+d", "shift+cmd+d", "CMD+SHIFT+D", " command + Shift + d "] {
            XCTAssertEqual(KeyChord.parse(text), expected, text)
        }
    }

    func testModifierAliases() {
        XCTAssertEqual(KeyChord.parse("opt+left"), KeyChord("left", option: true))
        XCTAssertEqual(KeyChord.parse("alt+left"), KeyChord("left", option: true))
        XCTAssertEqual(KeyChord.parse("ctrl+tab"), KeyChord("tab", control: true))
        XCTAssertEqual(KeyChord.parse("control+tab"), KeyChord("tab", control: true))
        XCTAssertEqual(KeyChord.parse("super+k"), KeyChord("k", command: true))
    }

    func testKeyAliasesFoldOntoCanonicalNames() {
        XCTAssertEqual(KeyChord.parse("cmd+return")?.key, "enter")
        XCTAssertEqual(KeyChord.parse("cmd+esc")?.key, "escape")
        XCTAssertEqual(KeyChord.parse("cmd+arrowdown")?.key, "down")
        XCTAssertEqual(KeyChord.parse("cmd+pgup")?.key, "pageup")
    }

    func testPlusIsBothSeparatorAndKey() {
        XCTAssertEqual(KeyChord.parse("cmd++"), KeyChord("+", command: true))
        XCTAssertEqual(KeyChord.parse("cmd+plus"), KeyChord("+", command: true))
    }

    func testRejectsGarbage() {
        for bad in ["", "   ", "cmd", "cmd+shift", "cmd+ab", "cmd+cmd+d", "cmd+d+e", "wat+d", "cmd+ +d"] {
            XCTAssertNil(KeyChord.parse(bad), bad)
        }
    }

    func testBareKeyParsesButCarriesNoModifier() {
        // Parseable, but Keymap refuses it — a bare key would eat terminal input.
        let chord = KeyChord.parse("d")
        XCTAssertEqual(chord, KeyChord("d"))
        XCTAssertFalse(chord!.hasModifier)
    }

    func testWireRoundTrips() {
        for text in ["cmd+shift+d", "ctrl+opt+shift+cmd+enter", "opt+left", "cmd+1", "cmd+,", "cmd+f5"] {
            let chord = KeyChord.parse(text)
            XCTAssertNotNil(chord, text)
            XCTAssertEqual(KeyChord.parse(chord!.wire), chord, text)
        }
    }

    func testWireIsCanonicalOrder() {
        XCTAssertEqual(KeyChord.parse("shift+cmd+ctrl+opt+d")?.wire, "ctrl+opt+shift+cmd+d")
    }

    func testDisplayUsesGlyphs() {
        XCTAssertEqual(KeyChord("enter", command: true, shift: true).display, "⇧⌘↩")
        XCTAssertEqual(KeyChord("left", command: true, option: true).display, "⌥⌘←")
        XCTAssertEqual(KeyChord("d", command: true).display, "⌘D")
    }

    /// Shift belongs in the modifier mask, never in the character: AppKit reads
    /// an uppercase key equivalent as implying shift, and an item asking for
    /// both simply never fires.
    func testMenuKeyEquivalentIsLowercaseWithShiftInTheMask() {
        let chord = KeyChord("d", command: true, shift: true)
        XCTAssertEqual(chord.menuKeyEquivalent, "d")
        XCTAssertTrue(chord.menuModifiers.contains(.shift))
        XCTAssertTrue(chord.menuModifiers.contains(.command))
    }

    func testMenuKeyEquivalentForNamedAndFunctionKeys() {
        XCTAssertEqual(KeyChord("enter", command: true).menuKeyEquivalent, "\r")
        XCTAssertEqual(KeyChord("left").menuKeyEquivalent, String(UnicodeScalar(NSLeftArrowFunctionKey)!))
        XCTAssertEqual(KeyChord("f3").menuKeyEquivalent, String(UnicodeScalar(NSF1FunctionKey + 2)!))
    }

    func testEveryDefaultChordSurvivesTheWireAndTheMenu() {
        for action in FlowAction.allCases {
            guard let chord = action.defaultChord else { continue }
            XCTAssertTrue(chord.hasModifier, "\(action.rawValue) has a bare-key default")
            XCTAssertEqual(KeyChord.parse(chord.wire), chord, action.rawValue)
            XCTAssertFalse(chord.menuKeyEquivalent.isEmpty, action.rawValue)
        }
    }
}

final class KeymapTests: XCTestCase {
    func testDefaultsHaveNoConflicts() {
        let conflicts = Keymap.default.conflicts
        XCTAssertTrue(conflicts.isEmpty, "shipped defaults collide: \(conflicts.mapValues { $0.map(\.rawValue) })")
    }

    /// The collision this pearl had to resolve: ⌘⇧↩ used to be Steer All
    /// Working, and is now Zoom Pane (every terminal's binding). If someone
    /// moves one of them back onto the other, this fails.
    func testZoomOwnsCommandShiftReturnAndSteerAllMovedOff() {
        XCTAssertEqual(Keymap.default.chord(for: .zoomPane), KeyChord("enter", command: true, shift: true))
        XCTAssertEqual(Keymap.default.chord(for: .steerAll), KeyChord("enter", command: true, option: true))
    }

    func testEveryActionHasATitle() {
        for action in FlowAction.allCases {
            XCTAssertFalse(action.title.isEmpty, action.rawValue)
            XCTAssertNotEqual(action.title, action.rawValue, "\(action.rawValue) fell through to its raw value")
        }
    }

    func testFocusSessionActionsAreNumberedOneThroughNine() {
        let indices = FlowAction.allCases.compactMap { FlowAction.focusSessionIndex($0) }.sorted()
        XCTAssertEqual(indices, Array(0..<9))
        XCTAssertEqual(FlowAction.focusSession3.title, "Focus Session 3")
        XCTAssertEqual(Keymap.default.chord(for: .focusSession3), KeyChord("3", command: true))
        XCTAssertNil(FlowAction.focusSessionIndex(.newTab))
    }

    func testParseOverridesAndUnbinds() {
        let map = Keymap.parse("""
        # a comment
        [keys]
        splitRight = "cmd+opt+d"   # trailing comment
        inbox = ""
        """)
        XCTAssertEqual(map.chord(for: .splitRight), KeyChord("d", command: true, option: true))
        XCTAssertNil(map.chord(for: .inbox))
        XCTAssertTrue(map.isCustom(.inbox))
        XCTAssertFalse(map.isCustom(.newTab))
        XCTAssertEqual(map.chord(for: .newTab), FlowAction.newTab.defaultChord)
        XCTAssertTrue(map.problems.isEmpty, "\(map.problems)")
    }

    func testOnlyTheKeysTableIsRead() {
        let map = Keymap.parse("""
        [other]
        splitRight = "cmd+opt+d"
        [keys]
        splitDown = "ctrl+j"
        [more]
        inbox = ""
        """)
        XCTAssertEqual(map.chord(for: .splitRight), FlowAction.splitRight.defaultChord)
        XCTAssertEqual(map.chord(for: .splitDown), KeyChord("j", control: true))
        XCTAssertEqual(map.chord(for: .inbox), FlowAction.inbox.defaultChord)
    }

    /// A bad line loses that line, never the map: shortcuts are how you reach a
    /// fleet of agents, and a stray bracket must not take them all away.
    func testBadLinesBecomeProblemsAndTheRestStillLoads() {
        let map = Keymap.parse("""
        [keys]
        splitRight = "cmd+opt+d"
        notAnAction = "cmd+j"
        splitDown = "gibberish"
        zoomPane = "x"
        this line has no equals
        """)
        XCTAssertEqual(map.chord(for: .splitRight), KeyChord("d", command: true, option: true))
        XCTAssertEqual(map.chord(for: .splitDown), FlowAction.splitDown.defaultChord)
        XCTAssertEqual(map.chord(for: .zoomPane), FlowAction.zoomPane.defaultChord, "a bare key must be refused")
        XCTAssertEqual(map.problems.count, 4, "\(map.problems)")
    }

    func testUnreadableFileFallsBackToDefaults() {
        let map = Keymap.load(from: URL(fileURLWithPath: "/nope/does/not/exist.toml"))
        XCTAssertEqual(map, Keymap.default)
        XCTAssertEqual(map.chord(for: .newTab), KeyChord("t", command: true))
    }

    func testRetypingADefaultIsNotAnOverride() {
        let map = Keymap.parse("""
        [keys]
        newTab = "cmd+t"
        """)
        XCTAssertFalse(map.isCustom(.newTab))
        XCTAssertEqual(map, Keymap.default)
    }

    func testSetToDefaultDropsTheOverride() {
        var map = Keymap()
        map.set(.newTab, to: KeyChord("y", command: true))
        XCTAssertTrue(map.isCustom(.newTab))
        map.set(.newTab, to: FlowAction.newTab.defaultChord)
        XCTAssertFalse(map.isCustom(.newTab))
    }

    func testConflictsAreReportedNotResolved() {
        var map = Keymap()
        map.set(.newTab, to: KeyChord("i", command: true)) // same as Inbox
        XCTAssertEqual(map.conflicts[KeyChord("i", command: true)].map { Set($0) }, Set<FlowAction>([.newTab, .inbox]))
        XCTAssertEqual(map.conflictPartners(of: .newTab), [.inbox])
        XCTAssertEqual(map.conflictPartners(of: .inbox), [.newTab])
        XCTAssertEqual(map.chord(for: .newTab), KeyChord("i", command: true), "a conflict must not silently drop the binding")
    }

    func testUnboundActionsNeverConflict() {
        var map = Keymap()
        map.set(.newTab, to: nil)
        map.set(.closeTab, to: nil)
        XCTAssertTrue(map.conflicts.isEmpty)
    }

    func testSerializedFileRoundTrips() {
        var map = Keymap()
        map.set(.splitRight, to: KeyChord("d", command: true, option: true))
        map.set(.inbox, to: nil)
        let reparsed = Keymap.parse(map.serialized)
        XCTAssertEqual(reparsed.chord(for: .splitRight), KeyChord("d", command: true, option: true))
        XCTAssertNil(reparsed.chord(for: .inbox))
        XCTAssertTrue(reparsed.isCustom(.inbox))
        XCTAssertEqual(reparsed.problems, [])
    }

    func testSerializedFileCarriesOnlyOverrides() {
        var map = Keymap()
        map.set(.splitRight, to: KeyChord("d", command: true, option: true))
        let body = map.serialized
        XCTAssertTrue(body.contains("splitRight = \"opt+cmd+d\""))
        XCTAssertFalse(body.contains("newTab ="), "untouched defaults must not be frozen into the file")
    }

    func testSaveAndLoadThroughDisk() throws {
        let url = FileManager.default.temporaryDirectory
            .appendingPathComponent("keys-\(UUID().uuidString)", isDirectory: true)
            .appendingPathComponent("keybindings.toml")
        var map = Keymap()
        map.set(.zoomPane, to: KeyChord("f", command: true, control: true))
        try map.save(to: url)
        defer { try? FileManager.default.removeItem(at: url.deletingLastPathComponent()) }
        XCTAssertEqual(Keymap.load(from: url).chord(for: .zoomPane), KeyChord("f", command: true, control: true))
    }

    func testResetAll() {
        var map = Keymap()
        map.set(.zoomPane, to: KeyChord("f", command: true))
        map.set(.inbox, to: nil)
        map.resetAll()
        XCTAssertEqual(map, Keymap.default)
    }
}

final class TOMLTests: XCTestCase {
    func testStripComment() {
        XCTAssertEqual(TOML.stripComment("a = \"b\" # hi").trimmingCharacters(in: .whitespaces), "a = \"b\"")
        XCTAssertEqual(TOML.stripComment("# whole line"), "")
        XCTAssertEqual(TOML.stripComment("a = \"#notacomment\""), "a = \"#notacomment\"")
    }

    func testKeyValue() {
        XCTAssertEqual(TOML.keyValue("a = \"b\"").map { [$0.0, $0.1] }, ["a", "b"])
        XCTAssertEqual(TOML.keyValue("  a=b  ").map { [$0.0, $0.1] }, ["a", "b"])
        XCTAssertEqual(TOML.keyValue("a = \"\"").map { [$0.0, $0.1] }, ["a", ""])
        XCTAssertNil(TOML.keyValue("no equals here"))
        XCTAssertNil(TOML.keyValue("= b"))
        XCTAssertNil(TOML.keyValue("a = \"unterminated"))
    }
}

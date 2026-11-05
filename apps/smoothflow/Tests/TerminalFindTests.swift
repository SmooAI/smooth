import AppKit
@testable import SmoothFlow
import XCTest

/// SMOODEV-4266: the find bar's rules, and the search itself against a real
/// libghostty surface in the mode the app uses (manual I/O, no window, no PTY).
@MainActor
final class TerminalFindTests: XCTestCase {
    func testCountLabelSaysWhatIsKnown() {
        XCTAssertEqual(TerminalFindState().countLabel, "", "no needle, nothing to count")
        XCTAssertEqual(TerminalFindState(needle: "x").countLabel, "…", "asked, not answered yet")
        XCTAssertEqual(TerminalFindState(needle: "x", total: 0).countLabel, "No matches")
        XCTAssertEqual(TerminalFindState(needle: "x", total: 12).countLabel, "12 found")
        XCTAssertEqual(TerminalFindState(needle: "x", total: 12, selected: 2).countLabel, "3 of 12", "selected is 0-based")
        XCTAssertEqual(TerminalFindState(needle: "x", total: 0, selected: 2).countLabel, "No matches", "a stale selection never outranks an empty result")
    }

    func testNegativeCountsMeanNone() {
        XCTAssertNil(TerminalFindState.reported(-1))
        XCTAssertEqual(TerminalFindState.reported(0), 0)
    }

    func testShortNeedlesWaitAndLongOnesDoNot() {
        XCTAssertEqual(TerminalFindState.debounce(forNeedle: ""), 0)
        XCTAssertGreaterThan(TerminalFindState.debounce(forNeedle: "a"), 0)
        XCTAssertGreaterThan(TerminalFindState.debounce(forNeedle: "ab"), 0)
        XCTAssertEqual(TerminalFindState.debounce(forNeedle: "abc"), 0)
    }

    func testBindingActionNames() {
        XCTAssertEqual(TerminalFindState.searchAction("a:b c"), "search:a:b c")
        XCTAssertEqual(TerminalFindState.searchAction(""), "search:")
        XCTAssertEqual(TerminalFindState.navigateAction(forward: true), "navigate_search:next")
        XCTAssertEqual(TerminalFindState.navigateAction(forward: false), "navigate_search:previous")
    }

    /// Spin the main run loop until `done`; libghostty's wakeups land there.
    private func wait(_ what: String, timeout: TimeInterval = 10, until done: () -> Bool) {
        let end = Date().addingTimeInterval(timeout)
        while !done(), Date() < end {
            RunLoop.current.run(until: Date().addingTimeInterval(0.02))
        }
        XCTAssertTrue(done(), "timed out waiting for \(what)")
    }

    func testARealSurfaceFindsCountsAndWalksMatches() throws {
        try XCTSkipIf(GhosttyRuntime.shared.app == nil, "no ghostty app in this host")
        let surface = TerminalSurfaceView(sessionId: "find-test")
        surface.feed(Data("alpha one\r\nbeta two\r\nalpha three\r\nalpha four\r\ngamma\r\n".utf8))

        surface.startFind(needle: "alpha")
        XCTAssertEqual(surface.findState?.needle, "alpha")
        wait("three matches") { surface.findState?.total == 3 }

        surface.navigateFind(forward: true)
        wait("a selected match") { surface.findState?.selected != nil }
        let first = surface.findState?.selected
        surface.navigateFind(forward: true)
        wait("the selection to move") { surface.findState?.selected != first }

        surface.needleEdited("gamma")
        wait("one match for a new needle") { surface.findState?.total == 1 }

        surface.needleEdited("no such text anywhere")
        wait("no matches") { surface.findState?.total == 0 }
        XCTAssertEqual(surface.findState?.countLabel, "No matches")

        surface.endFind()
        XCTAssertNil(surface.findState)
        surface.findTotalChanged(9)
        XCTAssertNil(surface.findState, "a late count after closing reopens nothing")
    }

    func testAnEmptyNeedleClearsTheCounts() throws {
        try XCTSkipIf(GhosttyRuntime.shared.app == nil, "no ghostty app in this host")
        let surface = TerminalSurfaceView(sessionId: "find-empty")
        surface.feed(Data("alpha\r\n".utf8))
        surface.startFind(needle: "alpha")
        wait("one match") { surface.findState?.total == 1 }
        surface.needleEdited("")
        XCTAssertNil(surface.findState?.total)
        XCTAssertEqual(surface.findState?.countLabel, "")
        surface.endFind()
    }

    /// The bar in a real window: typing in its field searches, Return walks
    /// the matches, Esc closes it and gives the keyboard back to the terminal.
    func testTheBarDrivesASearchFromItsField() throws {
        try XCTSkipIf(GhosttyRuntime.shared.app == nil, "no ghostty app in this host")
        let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 800, height: 400), styleMask: [.titled], backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false
        defer { window.close() }
        let surface = TerminalSurfaceView(sessionId: "find-bar")
        surface.frame = NSRect(x: 0, y: 0, width: 800, height: 400)
        window.contentView?.addSubview(surface)
        surface.feed(Data("needle one\r\nneedle two\r\n".utf8))

        surface.startFind()
        let field = try XCTUnwrap(window.firstResponder as? NSTextView, "the field's editor has the keyboard")
        field.insertText("needle", replacementRange: NSRange(location: 0, length: 0))
        XCTAssertEqual(surface.findState?.needle, "needle")
        wait("two matches") { surface.findState?.total == 2 }

        let bar = try XCTUnwrap(surface.subviews.compactMap { $0 as? TerminalFindBar }.first)
        XCTAssertTrue(bar.control(bar.field, textView: field, doCommandBy: #selector(NSResponder.insertNewline(_:))))
        wait("Return to select a match") { surface.findState?.selected != nil }
        XCTAssertTrue(bar.count.stringValue.hasSuffix(" of 2"), bar.count.stringValue)

        XCTAssertFalse(bar.control(bar.field, textView: field, doCommandBy: #selector(NSResponder.moveLeft(_:))), "ordinary editing stays the field's")
        XCTAssertTrue(bar.control(bar.field, textView: field, doCommandBy: #selector(NSResponder.cancelOperation(_:))))
        XCTAssertNil(surface.findState)
        XCTAssertTrue(bar.isHidden)
        XCTAssertTrue(window.firstResponder === surface, "the keyboard comes back to the terminal")

        surface.startFind()
        XCTAssertEqual(surface.findState?.needle, "", "a closed search does not come back")
        XCTAssertFalse(bar.isHidden)
        surface.endFind()
    }
}

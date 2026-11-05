@testable import SmoothFlow
import XCTest

/// SMOODEV-4127: tab actions must not rebuild what did not change.
@MainActor
final class SurfaceTabBarTests: XCTestCase {
    func testOnlyAddedOrRemovedTabsRebuildTheStrip() {
        let ab = ["a", "b"]
        XCTAssertEqual(SurfaceTabBar.change(from: (ab, 0), to: (ab, 0)), .none, "every render calls update; most change nothing")
        XCTAssertEqual(SurfaceTabBar.change(from: (ab, 0), to: (ab, 1)), .restyle, "switching tabs restyles")
        XCTAssertEqual(SurfaceTabBar.change(from: (ab, 0), to: (["a", "c"], 0)), .restyle, "a renamed tab restyles")
        XCTAssertEqual(SurfaceTabBar.change(from: (ab, 0), to: (["a", "b", "c"], 2)), .rebuild, "a new tab rebuilds")
        XCTAssertEqual(SurfaceTabBar.change(from: (ab, 1), to: (["a"], 0)), .rebuild, "a closed tab rebuilds")
    }

    func testMiddleClickHitsTheTabUnderThePointer() {
        let spans: [ClosedRange<CGFloat>] = [8...60, 62...120, 122...200]
        XCTAssertEqual(SurfaceTabBar.tabIndex(atX: 10, spans: spans), 0)
        XCTAssertEqual(SurfaceTabBar.tabIndex(atX: 120, spans: spans), 1)
        XCTAssertEqual(SurfaceTabBar.tabIndex(atX: 199, spans: spans), 2)
        XCTAssertNil(SurfaceTabBar.tabIndex(atX: 61, spans: spans), "the gap closes nothing")
        XCTAssertNil(SurfaceTabBar.tabIndex(atX: 300, spans: spans), "past the last tab (the + button) closes nothing")
    }

    func testTheStripKeepsItsCellsAcrossASwitchAndHidesForOneTab() {
        let bar = SurfaceTabBar()
        bar.update(titles: ["a"], active: 0)
        XCTAssertTrue(bar.isHidden)
        XCTAssertNil(bar.cell(at: 0))
        bar.update(titles: ["a", "b"], active: 0)
        XCTAssertFalse(bar.isHidden)
        let first = bar.cell(at: 0)
        XCTAssertNotNil(first)
        bar.update(titles: ["a", "b"], active: 1)
        XCTAssertTrue(bar.cell(at: 0) === first, "a switch restyles the same cell")
        bar.update(titles: ["a"], active: 0)
        XCTAssertTrue(bar.isHidden)
        bar.update(titles: ["z"], active: 0)
        XCTAssertTrue(bar.isHidden, "one tab, renamed: still no strip")
    }

    func testMiddleClickClosesTheTabUnderThePointer() throws {
        let bar = SurfaceTabBar()
        bar.frame = NSRect(x: 0, y: 0, width: 600, height: 26)
        var closed: [Int] = []
        bar.onClose = { closed.append($0) }
        bar.update(titles: ["one", "two"], active: 0)
        bar.layoutSubtreeIfNeeded()
        let second = try XCTUnwrap(bar.cell(at: 1))
        let mid = second.convert(NSPoint(x: second.bounds.midX, y: second.bounds.midY), to: nil)
        let event = try XCTUnwrap(NSEvent.mouseEvent(with: .otherMouseUp, location: mid, modifierFlags: [], timestamp: 0,
                                                    windowNumber: 0, context: nil, eventNumber: 0, clickCount: 1, pressure: 0))
        XCTAssertEqual(event.buttonNumber, 2, "AppKit's synthesized other-button event is the middle button")
        bar.otherMouseUp(with: event)
        XCTAssertEqual(closed, [1])
    }
}

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
        // `NSEvent.mouseEvent` always says button 0; only a CGEvent carries
        // the middle button. With no window, the location stays as given.
        let cg = try XCTUnwrap(CGEvent(mouseEventSource: nil, mouseType: .otherMouseUp, mouseCursorPosition: mid, mouseButton: .center))
        let event = try XCTUnwrap(NSEvent(cgEvent: cg))
        XCTAssertEqual(event.buttonNumber, 2)
        bar.otherMouseUp(with: event)
        XCTAssertEqual(closed, [1])
    }

    /// A tree first applied while it has no size (launch, a restored tab's
    /// first show) keeps its saved division instead of the split view's default.
    func testATreeBuiltBeforeItHasASizeStillGetsItsFractions() throws {
        let a = PaneID.next(), b = PaneID.next()
        let root = PaneNode.split(horizontal: true, first: .leaf(a), second: .leaf(b), fraction: 0.25)
        let tree = PaneTreeView()
        var written: [Double] = []
        tree.onFractionChanged = { f, _, _ in written.append(f) }
        tree.apply(root: root, focused: a, zoomed: nil)
        XCTAssertEqual(tree.bounds.width, 0)
        tree.frame = NSRect(x: 0, y: 0, width: 800, height: 400)
        tree.layoutSubtreeIfNeeded()
        let split = try XCTUnwrap(tree.subviews.first as? NSSplitView)
        XCTAssertEqual(split.arrangedSubviews[0].frame.width, 200, accuracy: 2)
        XCTAssertEqual(written.last ?? 0.25, 0.25, accuracy: 0.01, "the split view's default never outlives the saved fraction: \(written)")
    }
}

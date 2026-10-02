import XCTest
@testable import SmoothFlow

/// th-f958f2: middle-click on a fleet row is Close Out — the same action as
/// ⌘⌥W and the row's "Close Out…" menu item — and nothing else is.
final class FleetRowGestureTests: XCTestCase {
    func testMiddleButtonIsCloseOut() {
        XCTAssertEqual(FleetRowGesture.middleButton, 2, "NSEvent numbers the middle button 2")
        XCTAssertEqual(FleetRowGesture.action(forButton: 2), .closeOut)
    }

    func testLeftRightAndExtraButtonsStayWithTheList() {
        for b in [0, 1, 3, 4] {
            XCTAssertNil(FleetRowGesture.action(forButton: b), "button \(b) must not close anything")
        }
    }

    func testOnlyAClickOnThisRowInThisWindowCounts() {
        let row = CGRect(x: 0, y: 0, width: 220, height: 34)
        XCTAssertEqual(FleetRowGesture.action(forButton: 2, at: CGPoint(x: 10, y: 10), in: row, sameWindow: true), .closeOut)
        XCTAssertNil(FleetRowGesture.action(forButton: 2, at: CGPoint(x: 10, y: 40), in: row, sameWindow: true), "the row below")
        XCTAssertNil(FleetRowGesture.action(forButton: 2, at: CGPoint(x: 10, y: 10), in: row, sameWindow: false), "another window")
        XCTAssertNil(FleetRowGesture.action(forButton: 0, at: CGPoint(x: 10, y: 10), in: row, sameWindow: true), "a left click selects")
    }

    func testCloseOutIsTheSameActionTheKeymapAndMenuRun() {
        XCTAssertEqual(FlowAction.closeOut.title, "Close Out…", "it opens the confirmation sheet, never closes silently")
    }
}

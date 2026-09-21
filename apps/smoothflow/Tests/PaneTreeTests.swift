@testable import SmoothFlow
import XCTest

final class PaneNodeTests: XCTestCase {
    private func ids(_ n: Int) -> [PaneID] { (0..<n).map { _ in PaneID.next() } }

    func testSplitRightPutsTheNewPaneSecond() {
        let a = PaneID.next(), b = PaneID.next()
        let tree = PaneNode.leaf(a).splitting(a, direction: .right, newID: b)
        XCTAssertEqual(tree, .split(horizontal: true, first: .leaf(a), second: .leaf(b), fraction: 0.5))
        XCTAssertEqual(tree.leaves, [a, b])
    }

    func testSplitLeftPutsTheNewPaneFirst() {
        let a = PaneID.next(), b = PaneID.next()
        let tree = PaneNode.leaf(a).splitting(a, direction: .left, newID: b)
        XCTAssertEqual(tree, .split(horizontal: true, first: .leaf(b), second: .leaf(a), fraction: 0.5))
    }

    func testSplitUpAndDownAreVerticalStacks() {
        let a = PaneID.next(), b = PaneID.next()
        XCTAssertEqual(PaneNode.leaf(a).splitting(a, direction: .down, newID: b),
                       .split(horizontal: false, first: .leaf(a), second: .leaf(b), fraction: 0.5))
        XCTAssertEqual(PaneNode.leaf(a).splitting(a, direction: .up, newID: b),
                       .split(horizontal: false, first: .leaf(b), second: .leaf(a), fraction: 0.5))
    }

    func testSplittingANestedLeafOnlyTouchesThatLeaf() {
        let a = PaneID.next(), b = PaneID.next(), c = PaneID.next()
        let tree = PaneNode.leaf(a).splitting(a, direction: .right, newID: b).splitting(b, direction: .down, newID: c)
        XCTAssertEqual(tree.leaves, [a, b, c])
        XCTAssertEqual(tree, .split(horizontal: true, first: .leaf(a),
                                    second: .split(horizontal: false, first: .leaf(b), second: .leaf(c), fraction: 0.5),
                                    fraction: 0.5))
    }

    func testSplittingAnUnknownLeafIsANoOp() {
        let a = PaneID.next()
        let tree = PaneNode.leaf(a)
        XCTAssertEqual(tree.splitting(PaneID.next(), direction: .right, newID: PaneID.next()), tree)
    }

    func testRemovingCollapsesTheSplitThatHeldIt() {
        let a = PaneID.next(), b = PaneID.next()
        let tree = PaneNode.leaf(a).splitting(a, direction: .right, newID: b)
        XCTAssertEqual(tree.removing(b), .leaf(a))
        XCTAssertEqual(tree.removing(a), .leaf(b))
    }

    func testRemovingTheOnlyLeafYieldsNil() {
        let a = PaneID.next()
        XCTAssertNil(PaneNode.leaf(a).removing(a))
    }

    func testRemovingKeepsTheOtherSubtreeWhole() {
        let a = PaneID.next(), b = PaneID.next(), c = PaneID.next()
        let tree = PaneNode.leaf(a).splitting(a, direction: .right, newID: b).splitting(b, direction: .down, newID: c)
        XCTAssertEqual(tree.removing(a), .split(horizontal: false, first: .leaf(b), second: .leaf(c), fraction: 0.5))
        XCTAssertEqual(tree.removing(c)?.leaves, [a, b])
    }

    func testSettingFractionFindsTheSplitBetweenTwoPanes() {
        let a = PaneID.next(), b = PaneID.next(), c = PaneID.next()
        let tree = PaneNode.leaf(a).splitting(a, direction: .right, newID: b).splitting(b, direction: .down, newID: c)
        let dragged = tree.settingFraction(0.3, between: b, and: c)
        guard case let .split(_, _, second, outer) = dragged, case let .split(_, _, _, inner) = second else { return XCTFail("shape changed") }
        XCTAssertEqual(outer, 0.5, accuracy: 0.001, "the outer divider must not move")
        XCTAssertEqual(inner, 0.3, accuracy: 0.001)
    }

    func testFractionsAreClamped() {
        let a = PaneID.next(), b = PaneID.next()
        let tree = PaneNode.leaf(a).splitting(a, direction: .right, newID: b)
        guard case let .split(_, _, _, f) = tree.settingFraction(1.7, between: a, and: b) else { return XCTFail("shape changed") }
        XCTAssertEqual(f, 0.95, accuracy: 0.001)
        guard case let .split(_, _, _, g) = tree.settingFraction(-3, between: a, and: b) else { return XCTFail("shape changed") }
        XCTAssertEqual(g, 0.05, accuracy: 0.001)
    }

    func testEqualizedAndSkeletonNormalizeEveryFraction() {
        let a = PaneID.next(), b = PaneID.next(), c = PaneID.next()
        let tree = PaneNode.leaf(a).splitting(a, direction: .right, newID: b)
            .splitting(b, direction: .down, newID: c)
            .settingFraction(0.2, between: b, and: c)
        XCTAssertNotEqual(tree, tree.equalized)
        XCTAssertEqual(tree.skeleton, tree.equalized)
        XCTAssertEqual(tree.equalized.leaves, tree.leaves, "equalizing must not move a pane")
    }

    func testFramesSplitTheBoundsByFraction() {
        let a = PaneID.next(), b = PaneID.next()
        let tree = PaneNode.leaf(a).splitting(a, direction: .right, newID: b).settingFraction(0.25, between: a, and: b)
        let frames = tree.frames(in: CGRect(x: 0, y: 0, width: 400, height: 100))
        XCTAssertEqual(frames[a], CGRect(x: 0, y: 0, width: 100, height: 100))
        XCTAssertEqual(frames[b], CGRect(x: 100, y: 0, width: 300, height: 100))
    }

    /// `first` is the UPPER pane of a vertical split, so it takes the high-y
    /// slice — the arrow keys read these frames, and getting it backwards puts
    /// "focus down" on the pane above.
    func testVerticalSplitPutsFirstOnTop() {
        let a = PaneID.next(), b = PaneID.next()
        let tree = PaneNode.leaf(a).splitting(a, direction: .down, newID: b)
        let frames = tree.frames(in: CGRect(x: 0, y: 0, width: 100, height: 200))
        XCTAssertEqual(frames[a], CGRect(x: 0, y: 100, width: 100, height: 100))
        XCTAssertEqual(frames[b], CGRect(x: 0, y: 0, width: 100, height: 100))
    }
}

final class DirectionalFocusTests: XCTestCase {
    /// A 2×2 grid: tl tr / bl br, in a 200×200 box.
    private func grid() -> (tl: PaneID, tr: PaneID, bl: PaneID, br: PaneID, frames: [PaneID: CGRect]) {
        let tl = PaneID.next(), tr = PaneID.next(), bl = PaneID.next(), br = PaneID.next()
        let frames = [
            tl: CGRect(x: 0, y: 100, width: 100, height: 100),
            tr: CGRect(x: 100, y: 100, width: 100, height: 100),
            bl: CGRect(x: 0, y: 0, width: 100, height: 100),
            br: CGRect(x: 100, y: 0, width: 100, height: 100),
        ]
        return (tl, tr, bl, br, frames)
    }

    func testEachDirectionFromEachCorner() {
        let g = grid()
        XCTAssertEqual(paneInDirection(.right, from: g.tl, frames: g.frames), g.tr)
        XCTAssertEqual(paneInDirection(.down, from: g.tl, frames: g.frames), g.bl)
        XCTAssertEqual(paneInDirection(.left, from: g.br, frames: g.frames), g.bl)
        XCTAssertEqual(paneInDirection(.up, from: g.br, frames: g.frames), g.tr)
    }

    func testNoWrapAround() {
        let g = grid()
        XCTAssertNil(paneInDirection(.left, from: g.tl, frames: g.frames))
        XCTAssertNil(paneInDirection(.up, from: g.tl, frames: g.frames))
        XCTAssertNil(paneInDirection(.right, from: g.br, frames: g.frames))
        XCTAssertNil(paneInDirection(.down, from: g.br, frames: g.frames))
    }

    func testNearestEdgeWinsOverFartherOne() {
        let a = PaneID.next(), near = PaneID.next(), far = PaneID.next()
        let frames = [
            a: CGRect(x: 0, y: 0, width: 50, height: 100),
            near: CGRect(x: 50, y: 0, width: 50, height: 100),
            far: CGRect(x: 100, y: 0, width: 50, height: 100),
        ]
        XCTAssertEqual(paneInDirection(.right, from: a, frames: frames), near)
    }

    /// A tall pane facing two stacked ones: the tie on distance breaks by
    /// centerline, so ⌘⌥→ lands on the neighbour you are level with.
    func testTieBreaksOnCenterline() {
        let tall = PaneID.next(), top = PaneID.next(), bottom = PaneID.next()
        let frames = [
            tall: CGRect(x: 0, y: 0, width: 100, height: 40),
            top: CGRect(x: 100, y: 100, width: 100, height: 100),
            bottom: CGRect(x: 100, y: 0, width: 100, height: 100),
        ]
        XCTAssertEqual(paneInDirection(.right, from: tall, frames: frames), bottom)
    }

    func testUnknownOriginIsNil() {
        let g = grid()
        XCTAssertNil(paneInDirection(.right, from: PaneID.next(), frames: g.frames))
    }

    func testSinglePaneHasNoNeighbour() {
        let a = PaneID.next()
        for d in SplitDirection.allCases {
            XCTAssertNil(paneInDirection(d, from: a, frames: [a: CGRect(x: 0, y: 0, width: 10, height: 10)]))
        }
    }
}

final class SurfaceTabTests: XCTestCase {
    private let bounds = CGRect(x: 0, y: 0, width: 400, height: 400)

    func testSplitFocusesTheNewPaneAndCarriesTheSession() {
        var tab = SurfaceTab(id: 1, pane: PaneID.next())
        tab.sessions[tab.focused] = "s1"
        let old = tab.focused
        let fresh = tab.split(.right)
        XCTAssertEqual(tab.focused, fresh)
        XCTAssertEqual(tab.sessions[fresh], "s1", "a split shows what it was split from")
        XCTAssertEqual(tab.sessions[old], "s1")
        XCTAssertEqual(tab.panes.count, 2)
    }

    func testSplitClearsZoom() {
        var tab = SurfaceTab(id: 1, pane: PaneID.next())
        _ = tab.split(.right)
        tab.toggleZoom()
        XCTAssertNotNil(tab.zoomed)
        _ = tab.split(.down)
        XCTAssertNil(tab.zoomed)
    }

    func testCloseFocusedCollapsesAndRefocuses() {
        var tab = SurfaceTab(id: 1, pane: PaneID.next())
        let first = tab.focused
        let second = tab.split(.right)
        tab.sessions[second] = "s2"
        XCTAssertTrue(tab.closeFocused())
        XCTAssertEqual(tab.panes, [first])
        XCTAssertEqual(tab.focused, first)
        XCTAssertNil(tab.sessions[second], "a closed pane must not keep its session mapping")
    }

    func testCloseFocusedRefusesTheLastPane() {
        var tab = SurfaceTab(id: 1, pane: PaneID.next())
        XCTAssertFalse(tab.closeFocused(), "the tab closes instead — that is the caller's call")
        XCTAssertEqual(tab.panes.count, 1)
    }

    func testClosingAZoomedPaneUnzooms() {
        var tab = SurfaceTab(id: 1, pane: PaneID.next())
        _ = tab.split(.right)
        tab.toggleZoom()
        XCTAssertTrue(tab.closeFocused())
        XCTAssertNil(tab.zoomed)
    }

    func testFocusMovesByDirection() {
        var tab = SurfaceTab(id: 1, pane: PaneID.next())
        let left = tab.focused
        let right = tab.split(.right)
        tab.focus(.left, in: bounds)
        XCTAssertEqual(tab.focused, left)
        tab.focus(.right, in: bounds)
        XCTAssertEqual(tab.focused, right)
        tab.focus(.right, in: bounds)
        XCTAssertEqual(tab.focused, right, "no wrap-around at the edge")
    }

    func testFocusIsInertWhileZoomed() {
        var tab = SurfaceTab(id: 1, pane: PaneID.next())
        _ = tab.split(.right)
        tab.toggleZoom()
        let zoomedOn = tab.focused
        tab.focus(.left, in: bounds)
        XCTAssertEqual(tab.focused, zoomedOn, "there is nowhere to go inside a zoomed pane")
    }

    func testZoomNeedsMoreThanOnePane() {
        var tab = SurfaceTab(id: 1, pane: PaneID.next())
        tab.toggleZoom()
        XCTAssertNil(tab.zoomed)
    }

    func testZoomTogglesBackOff() {
        var tab = SurfaceTab(id: 1, pane: PaneID.next())
        let shape = { (t: SurfaceTab) in t.root }
        _ = tab.split(.down)
        let before = shape(tab)
        tab.toggleZoom()
        XCTAssertEqual(shape(tab), before, "zoom must not touch the layout underneath")
        tab.toggleZoom()
        XCTAssertNil(tab.zoomed)
    }

    func testEqualizeResetsFractionsAndZoom() {
        var tab = SurfaceTab(id: 1, pane: PaneID.next())
        let a = tab.focused
        let b = tab.split(.right)
        tab.root = tab.root.settingFraction(0.9, between: a, and: b)
        tab.toggleZoom()
        tab.equalize()
        XCTAssertNil(tab.zoomed)
        XCTAssertEqual(tab.root, tab.root.equalized)
    }

    func testTitleFallsBackWhenNoSession() {
        var tab = SurfaceTab(id: 1, pane: PaneID.next())
        XCTAssertEqual(tab.title { _ in nil }, "empty")
        _ = tab.split(.right)
        XCTAssertEqual(tab.title { _ in nil }, "2 panes")
        tab.sessions[tab.focused] = "s1"
        XCTAssertEqual(tab.title { _ in "th-123456" }, "th-123456")
    }

    func testPaneIDsAreNeverReused() {
        var seen = Set<PaneID>()
        for _ in 0..<200 { XCTAssertTrue(seen.insert(PaneID.next()).inserted) }
    }
}

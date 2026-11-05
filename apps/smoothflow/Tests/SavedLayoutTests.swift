@testable import SmoothFlow
import XCTest

/// SMOODEV-4238: tabs and splits survive a relaunch.
final class SavedLayoutTests: XCTestCase {
    /// Two tabs: a three-pane tab (left | top/bottom) with a dragged divider,
    /// focus on the bottom pane and two sessions; and a single-pane tab.
    private func sampleTabs() -> [SurfaceTab] {
        var t = SurfaceTab(id: 1, pane: PaneID.next())
        let left = t.focused
        t.sessions[left] = "fs-aaaaaaaa"
        let right = t.split(.right)
        t.sessions[right] = "fs-bbbbbbbb"
        let bottom = t.split(.down)
        t.sessions[bottom] = nil
        t.root = t.root.settingFraction(0.3, between: left, and: right)
        t.focused = bottom
        var u = SurfaceTab(id: 2, pane: PaneID.next())
        u.sessions[u.focused] = "fs-cccccccc"
        return [t, u]
    }

    private func roundTrip(_ layout: SavedLayout) throws -> SavedLayout {
        try JSONDecoder().decode(SavedLayout.self, from: JSONEncoder().encode(layout))
    }

    func testEveryTabAndSplitComesBackWithFreshPaneIds() throws {
        let tabs = sampleTabs()
        let saved = try roundTrip(SavedLayout(tabs, active: 1))
        let (restored, active) = try XCTUnwrap(saved.restore(firstTabId: 10))
        XCTAssertEqual(active, 1)
        XCTAssertEqual(restored.map(\.id), [10, 11])
        for (old, new) in zip(tabs, restored) {
            XCTAssertEqual(new.root.skeleton, Self.relabel(old.root.skeleton, from: old.panes, to: new.panes), "same shape")
            XCTAssertTrue(Set(new.panes).isDisjoint(with: old.panes), "pane ids are process-local: fresh ones")
            XCTAssertEqual(old.panes.map { old.sessions[$0] }, new.panes.map { new.sessions[$0] }, "each pane shows the same session")
            XCTAssertEqual(old.panes.firstIndex(of: old.focused), new.panes.firstIndex(of: new.focused))
        }
        XCTAssertEqual(restored[0].root, Self.relabel(tabs[0].root, from: tabs[0].panes, to: restored[0].panes), "the dragged divider too")
        XCTAssertEqual(saved.sessionIds, ["fs-aaaaaaaa", "fs-bbbbbbbb", "fs-cccccccc"])
    }

    func testZoomIsKept() throws {
        var t = sampleTabs()[0]
        t.toggleZoom()
        let restored = try XCTUnwrap(try roundTrip(SavedLayout([t], active: 0)).restore(firstTabId: 1)).tabs[0]
        XCTAssertEqual(restored.zoomed, restored.focused)
    }

    func testTheJSONIsReadable() throws {
        let data = try JSONEncoder().encode(SavedLayout(sampleTabs(), active: 0))
        let json = try XCTUnwrap(JSONSerialization.jsonObject(with: data) as? [String: Any])
        XCTAssertEqual(json["version"] as? Int, 1)
        let first = try XCTUnwrap((json["tabs"] as? [[String: Any]])?.first)
        XCTAssertEqual(first["sessions"] as? [String: String], ["0": "fs-aaaaaaaa", "1": "fs-bbbbbbbb"], "an object keyed by leaf position")
    }

    func testNothingUsableRestoresNothing() {
        XCTAssertNil(SavedLayout(tabs: [], active: 0).restore(firstTabId: 1))
        var future = SavedLayout(sampleTabs(), active: 0)
        future.version = 99
        XCTAssertNil(future.restore(firstTabId: 1), "a layout from a newer app is not guessed at")
    }

    /// Hand-edited or stale values are clamped, never trusted to index.
    func testOutOfRangeValuesAreClamped() throws {
        let tab = SavedTab(root: .split(horizontal: true, first: .leaf, second: .leaf, fraction: 7), focused: 9, zoomed: 5,
                           sessions: [0: "fs-aaaaaaaa", 4: "fs-dddddddd"])
        let (tabs, active) = try XCTUnwrap(SavedLayout(tabs: [tab], active: 3).restore(firstTabId: 1))
        XCTAssertEqual(active, 0)
        let t = tabs[0]
        XCTAssertEqual(t.focused, t.panes[1])
        XCTAssertNil(t.zoomed)
        XCTAssertEqual(t.sessions.count, 1)
        guard case let .split(_, _, _, f) = t.root else { return XCTFail() }
        XCTAssertEqual(f, 0.95)
    }

    func testSaveAndLoadThroughDefaults() throws {
        let suite = "ai.smoo.smoothflow.tests.layout.\(UUID().uuidString)"
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suite))
        defer { defaults.removePersistentDomain(forName: suite) }
        XCTAssertNil(LayoutPersistence.load(defaults))
        let layout = SavedLayout(sampleTabs(), active: 1)
        LayoutPersistence.save(layout, to: defaults)
        XCTAssertEqual(LayoutPersistence.load(defaults), layout)
        defaults.set(Data("not json".utf8), forKey: LayoutPersistence.key)
        XCTAssertNil(LayoutPersistence.load(defaults), "a corrupt value is ignored, not fatal")
    }

    /// `tree` with each leaf id from `from` swapped for the same position's in `to`.
    private static func relabel(_ tree: PaneNode, from: [PaneID], to: [PaneID]) -> PaneNode {
        switch tree {
        case let .leaf(id): .leaf(to[from.firstIndex(of: id)!])
        case let .split(h, a, b, f): .split(horizontal: h, first: relabel(a, from: from, to: to), second: relabel(b, from: from, to: to), fraction: f)
        }
    }
}

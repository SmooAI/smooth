import Foundation

/// The surface tabs and their pane trees, as they are written to disk so a
/// relaunch (an app quit, a crash, a reboot) brings every tab and split back
/// where it was (SMOODEV-4238, cmux-style restore).
///
/// Only the shape and which session each pane shows are kept. Pane ids are
/// process-local (`PaneID.next()`), so a saved tree names its leaves by
/// position, in `PaneNode.leaves` order, and restoring hands out fresh ids.
/// The sessions themselves are the engine's business: a live one is still
/// running (the `pty` host outlives the app), a dead agent is resumed through
/// its harness, and a shell reopens in its last directory. A pane whose
/// session the engine no longer knows comes back empty, never dropped.
struct SavedLayout: Codable, Equatable {
    static let version = 1

    var version: Int = SavedLayout.version
    var tabs: [SavedTab]
    var active: Int

    init(tabs: [SavedTab], active: Int) {
        self.tabs = tabs
        self.active = active
    }

    init(_ tabs: [SurfaceTab], active: Int) {
        self.init(tabs: tabs.map(SavedTab.init), active: active)
    }

    /// The tabs to show again, with fresh pane ids, and which one is active.
    /// Nil when there is nothing usable (no tabs, an unknown version).
    func restore(firstTabId: Int) -> (tabs: [SurfaceTab], active: Int)? {
        guard version == Self.version, !tabs.isEmpty else { return nil }
        let restored = tabs.enumerated().map { i, t in t.restore(id: firstTabId + i) }
        return (restored, min(max(active, 0), restored.count - 1))
    }

    /// Every session a pane shows, for knowing when they have all arrived.
    var sessionIds: Set<String> { Set(tabs.flatMap { $0.sessions.values }) }
}

/// One tab: its tree, and focus, zoom and sessions by leaf position.
struct SavedTab: Codable, Equatable {
    var root: SavedNode
    var focused: Int
    var zoomed: Int?
    /// Leaf position → session id. Encoded with string keys so the JSON is an
    /// object, not Swift's alternating key/value array.
    var sessions: [String: String]

    init(root: SavedNode, focused: Int, zoomed: Int? = nil, sessions: [Int: String] = [:]) {
        self.root = root
        self.focused = focused
        self.zoomed = zoomed
        self.sessions = Dictionary(uniqueKeysWithValues: sessions.map { (String($0.key), $0.value) })
    }

    init(_ tab: SurfaceTab) {
        let leaves = tab.root.leaves
        let index = { (id: PaneID) in leaves.firstIndex(of: id) }
        var sessions: [Int: String] = [:]
        for (pane, sid) in tab.sessions {
            if let i = index(pane) { sessions[i] = sid }
        }
        self.init(root: SavedNode(tab.root), focused: index(tab.focused) ?? 0, zoomed: tab.zoomed.flatMap(index), sessions: sessions)
    }

    func restore(id: Int) -> SurfaceTab {
        let leafCount = root.leafCount
        let ids = (0 ..< leafCount).map { _ in PaneID.next() }
        var tab = SurfaceTab(id: id, pane: ids[0])
        var cursor = 0
        tab.root = root.restore(ids: ids, cursor: &cursor)
        tab.focused = ids[min(max(focused, 0), leafCount - 1)]
        tab.zoomed = zoomed.flatMap { ids.indices.contains($0) && leafCount > 1 ? ids[$0] : nil }
        for (key, sid) in sessions {
            if let i = Int(key), ids.indices.contains(i) { tab.sessions[ids[i]] = sid }
        }
        return tab
    }
}

/// `PaneNode` without its ids.
indirect enum SavedNode: Codable, Equatable {
    case leaf
    case split(horizontal: Bool, first: SavedNode, second: SavedNode, fraction: Double)

    init(_ node: PaneNode) {
        switch node {
        case .leaf: self = .leaf
        case let .split(h, a, b, f): self = .split(horizontal: h, first: SavedNode(a), second: SavedNode(b), fraction: f)
        }
    }

    var leafCount: Int {
        switch self {
        case .leaf: 1
        case let .split(_, a, b, _): a.leafCount + b.leafCount
        }
    }

    /// Rebuild the tree, taking leaf ids from `ids` in order.
    func restore(ids: [PaneID], cursor: inout Int) -> PaneNode {
        switch self {
        case .leaf:
            defer { cursor += 1 }
            return .leaf(ids[cursor])
        case let .split(h, a, b, f):
            let first = a.restore(ids: ids, cursor: &cursor)
            let second = b.restore(ids: ids, cursor: &cursor)
            return .split(horizontal: h, first: first, second: second, fraction: min(max(f, 0.05), 0.95))
        }
    }
}

/// Where the layout lives: one JSON value in the app's defaults.
enum LayoutPersistence {
    static let key = "surfaceLayout"

    static func load(_ defaults: UserDefaults = .standard) -> SavedLayout? {
        guard let data = defaults.data(forKey: key) else { return nil }
        return try? JSONDecoder().decode(SavedLayout.self, from: data)
    }

    static func save(_ layout: SavedLayout, to defaults: UserDefaults = .standard) {
        guard let data = try? JSONEncoder().encode(layout) else { return }
        defaults.set(data, forKey: key)
    }
}

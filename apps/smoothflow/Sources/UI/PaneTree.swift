import Foundation

enum SplitDirection: String, Sendable, CaseIterable {
    case left, right, up, down

    var isHorizontal: Bool { self == .left || self == .right }
    /// True when the new pane goes after the existing one in layout order.
    var insertsAfter: Bool { self == .right || self == .down }
}

/// The pane layout of one tab: a binary tree whose leaves are panes.
///
/// A tree, not a list, because "split the focused pane downward" has no meaning
/// in a flat row — which is why SmoothFlow's old ⌘D could only ever add another
/// column. Leaves carry an id so the view can keep a surface alive across a
/// relayout, and a fraction rides on each split so a dragged divider survives
/// one too.
indirect enum PaneNode: Equatable, Sendable {
    case leaf(PaneID)
    /// `fraction` is the share of the axis taken by `first`.
    case split(horizontal: Bool, first: PaneNode, second: PaneNode, fraction: Double)

    var leaves: [PaneID] {
        switch self {
        case let .leaf(id): [id]
        case let .split(_, a, b, _): a.leaves + b.leaves
        }
    }

    var count: Int { leaves.count }

    func contains(_ id: PaneID) -> Bool {
        switch self {
        case let .leaf(l): l == id
        case let .split(_, a, b, _): a.contains(id) || b.contains(id)
        }
    }

    /// Split the leaf `target` in `direction`, putting `newID` on that side.
    /// Returns the tree unchanged when `target` is not in it.
    func splitting(_ target: PaneID, direction: SplitDirection, newID: PaneID) -> PaneNode {
        switch self {
        case let .leaf(id):
            guard id == target else { return self }
            let existing = PaneNode.leaf(id)
            let fresh = PaneNode.leaf(newID)
            let (first, second) = direction.insertsAfter ? (existing, fresh) : (fresh, existing)
            return .split(horizontal: direction.isHorizontal, first: first, second: second, fraction: 0.5)
        case let .split(h, a, b, f):
            return .split(horizontal: h, first: a.splitting(target, direction: direction, newID: newID),
                          second: b.splitting(target, direction: direction, newID: newID), fraction: f)
        }
    }

    /// Remove a leaf, collapsing the split that held it. Nil when the tree was
    /// that single leaf — the caller decides whether an empty tab survives.
    func removing(_ target: PaneID) -> PaneNode? {
        switch self {
        case let .leaf(id):
            return id == target ? nil : self
        case let .split(h, a, b, f):
            if let a2 = a.removing(target) {
                guard let b2 = b.removing(target) else { return a2 }
                return .split(horizontal: h, first: a2, second: b2, fraction: f)
            }
            return b
        }
    }

    /// Set the divider fraction of the split that directly separates `a` from
    /// `b` — how a dragged divider is written back.
    func settingFraction(_ value: Double, between a: PaneID, and b: PaneID) -> PaneNode {
        switch self {
        case .leaf:
            return self
        case let .split(h, first, second, f):
            if first.contains(a), second.contains(b) {
                return .split(horizontal: h, first: first, second: second, fraction: min(max(value, 0.05), 0.95))
            }
            return .split(horizontal: h,
                          first: first.settingFraction(value, between: a, and: b),
                          second: second.settingFraction(value, between: a, and: b), fraction: f)
        }
    }

    /// The tree with every fraction normalized — two trees with the same
    /// skeleton need no rebuild, only a divider nudge. Same shape as
    /// `equalized`, different job.
    var skeleton: PaneNode { equalized }

    /// Every divider back to an even split.
    var equalized: PaneNode {
        switch self {
        case .leaf: self
        case let .split(h, a, b, _): .split(horizontal: h, first: a.equalized, second: b.equalized, fraction: 0.5)
        }
    }

    /// The leaf frames this tree produces inside `bounds`, which is what
    /// directional focus needs and the only geometry the model computes. The
    /// view lays out with real NSSplitViews; both agree because both read the
    /// same fractions.
    func frames(in bounds: CGRect) -> [PaneID: CGRect] {
        switch self {
        case let .leaf(id):
            return [id: bounds]
        case let .split(h, a, b, f):
            let (r1, r2): (CGRect, CGRect)
            if h {
                let w = bounds.width * f
                r1 = CGRect(x: bounds.minX, y: bounds.minY, width: w, height: bounds.height)
                r2 = CGRect(x: bounds.minX + w, y: bounds.minY, width: bounds.width - w, height: bounds.height)
            } else {
                // Top-down: `first` is the upper pane, so it takes the high-y
                // slice in AppKit's flipped-from-intuition coordinate space.
                let hgt = bounds.height * f
                r1 = CGRect(x: bounds.minX, y: bounds.maxY - hgt, width: bounds.width, height: hgt)
                r2 = CGRect(x: bounds.minX, y: bounds.minY, width: bounds.width, height: bounds.height - hgt)
            }
            return a.frames(in: r1).merging(b.frames(in: r2)) { l, _ in l }
        }
    }
}

/// A pane's identity. Opaque and monotonic — never an index, so removing a
/// pane cannot silently retarget another one's surface.
struct PaneID: Hashable, Sendable, CustomStringConvertible {
    let value: Int
    var description: String { "pane\(value)" }

    private static var counter = 0
    static func next() -> PaneID {
        counter += 1
        return PaneID(value: counter)
    }
}

/// Which leaf sits in `direction` from `from`, judged by frame geometry:
/// among the panes that actually lie that way, the one whose edge is nearest,
/// breaking ties by centerline distance on the other axis. That is the rule
/// every tiling window manager uses, and it is the one that feels right when
/// the layout is not a neat grid.
func paneInDirection(_ direction: SplitDirection, from: PaneID, frames: [PaneID: CGRect]) -> PaneID? {
    guard let origin = frames[from] else { return nil }
    var best: (id: PaneID, primary: CGFloat, secondary: CGFloat)?
    for (id, r) in frames where id != from {
        let primary: CGFloat
        let secondary: CGFloat
        switch direction {
        case .left:
            guard r.midX < origin.midX else { continue }
            primary = origin.minX - r.maxX
            secondary = abs(r.midY - origin.midY)
        case .right:
            guard r.midX > origin.midX else { continue }
            primary = r.minX - origin.maxX
            secondary = abs(r.midY - origin.midY)
        case .up:
            guard r.midY > origin.midY else { continue }
            primary = r.minY - origin.maxY
            secondary = abs(r.midX - origin.midX)
        case .down:
            guard r.midY < origin.midY else { continue }
            primary = origin.minY - r.maxY
            secondary = abs(r.midX - origin.midX)
        }
        // Panes that overlap the origin on the primary axis (nested splits)
        // give a small negative distance; that is fine, they are still "that
        // way", and a nearer edge still wins.
        if let b = best, (b.primary, b.secondary) <= (primary, secondary) { continue }
        best = (id: id, primary: primary, secondary: secondary)
    }
    return best?.id
}

/// One tab: a pane layout, which pane is focused, and whether it is zoomed.
struct SurfaceTab: Identifiable, Equatable, Sendable {
    let id: Int
    var root: PaneNode
    var focused: PaneID
    /// Set while one pane fills the tab; the layout is untouched underneath.
    var zoomed: PaneID?
    /// Which session each pane shows. A pane with no entry is empty.
    var sessions: [PaneID: String] = [:]

    init(id: Int, pane: PaneID) {
        self.id = id
        root = .leaf(pane)
        focused = pane
    }

    var panes: [PaneID] { root.leaves }

    /// The tab strip's label: the focused pane's session, or the pane count.
    func title(sessionTitle: (String) -> String?) -> String {
        if let sid = sessions[focused], let t = sessionTitle(sid) { return t }
        return panes.count > 1 ? "\(panes.count) panes" : "empty"
    }

    mutating func split(_ direction: SplitDirection) -> PaneID {
        let fresh = PaneID.next()
        root = root.splitting(focused, direction: direction, newID: fresh)
        // A new pane starts on the session the pane it came from was showing,
        // which is what "split this" means everywhere else.
        if let sid = sessions[focused] { sessions[fresh] = sid }
        focused = fresh
        zoomed = nil
        return fresh
    }

    /// Close the focused pane. False when it was the last one (the caller
    /// closes the tab instead).
    mutating func closeFocused() -> Bool {
        guard panes.count > 1 else { return false }
        let gone = focused
        let ordered = panes
        let next = ordered.firstIndex(of: gone).map { ordered[$0 == 0 ? 1 : $0 - 1] } ?? ordered[0]
        guard let root2 = root.removing(gone) else { return false }
        root = root2
        sessions.removeValue(forKey: gone)
        focused = root.contains(next) ? next : (root.leaves.first ?? next)
        if zoomed == gone { zoomed = nil }
        return true
    }

    mutating func focus(_ direction: SplitDirection, in bounds: CGRect) {
        guard zoomed == nil else { return }
        if let next = paneInDirection(direction, from: focused, frames: root.frames(in: bounds)) { focused = next }
    }

    mutating func toggleZoom() {
        guard panes.count > 1 else { zoomed = nil; return }
        zoomed = zoomed == nil ? focused : nil
    }

    mutating func equalize() {
        root = root.equalized
        zoomed = nil
    }
}

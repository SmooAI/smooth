import AppKit

/// Renders a `PaneNode` as nested `NSSplitView`s.
///
/// The hierarchy is rebuilt whenever the tree changes shape, but the
/// `SessionPane` views are not: they are kept by `PaneID` and re-parented, so
/// the terminal surface inside one survives a split, a close, a zoom and a tab
/// switch. Only the fractions travel back out — a dragged divider writes to the
/// model without a rebuild, or the drag would fight the relayout.
@MainActor
final class PaneTreeView: NSView, NSSplitViewDelegate {
    /// A divider moved: the model should record `fraction` for the split
    /// separating these two panes.
    var onFractionChanged: ((Double, PaneID, PaneID) -> Void)?
    var onPaneActivated: ((PaneID) -> Void)?

    private var panes: [PaneID: SessionPane] = [:]
    private var shape: PaneNode?
    private var zoomed: PaneID?
    private var focused: PaneID?
    /// Each generated split view, with the pair of panes its divider separates.
    private var splits: [ObjectIdentifier: (first: PaneID, second: PaneID)] = [:]
    private var applyingLayout = false

    /// The live pane view for `id`, created on demand. Callers host session
    /// surfaces in it.
    func pane(_ id: PaneID) -> SessionPane {
        if let p = panes[id] { return p }
        let p = SessionPane()
        p.onActivate = { [weak self] in self?.onPaneActivated?(id) }
        panes[id] = p
        return p
    }

    /// `redividing` re-pushes the fractions without a rebuild — what
    /// "Equalize Panes" needs, since equalizing changes no structure.
    func apply(root: PaneNode, focused: PaneID, zoomed: PaneID?, redividing: Bool = false) {
        let effective: PaneNode = zoomed.map { .leaf($0) } ?? root
        for id in panes.keys where !root.contains(id) { panes.removeValue(forKey: id) }
        if effective.skeleton != shape {
            shape = effective.skeleton
            rebuild(effective)
        } else if redividing, let v = subviews.first {
            applyingLayout = true
            applyFractions(effective, in: v)
            applyingLayout = false
        }
        self.zoomed = zoomed
        self.focused = focused
        for (id, p) in panes { p.setFocused(id == focused) }
    }

    private func rebuild(_ node: PaneNode) {
        applyingLayout = true
        defer { applyingLayout = false }
        splits.removeAll()
        subviews.forEach { $0.removeFromSuperview() }
        let v = build(node)
        v.translatesAutoresizingMaskIntoConstraints = false
        addSubview(v)
        NSLayoutConstraint.activate([
            v.leadingAnchor.constraint(equalTo: leadingAnchor), v.trailingAnchor.constraint(equalTo: trailingAnchor),
            v.topAnchor.constraint(equalTo: topAnchor), v.bottomAnchor.constraint(equalTo: bottomAnchor),
        ])
        layoutSubtreeIfNeeded()
        applyFractions(node, in: v)
    }

    private func build(_ node: PaneNode) -> NSView {
        switch node {
        case let .leaf(id):
            let p = pane(id)
            p.removeFromSuperview()
            return p
        case let .split(horizontal, a, b, _):
            let sv = NSSplitView()
            sv.isVertical = horizontal
            sv.dividerStyle = .thin
            sv.delegate = self
            sv.addArrangedSubview(build(a))
            sv.addArrangedSubview(build(b))
            if let fa = a.leaves.first, let fb = b.leaves.first {
                splits[ObjectIdentifier(sv)] = (fa, fb)
            }
            return sv
        }
    }

    /// Push the model's fractions onto the freshly built split views. Done
    /// after layout so the split view knows its own size.
    private func applyFractions(_ node: PaneNode, in view: NSView) {
        guard case let .split(horizontal, a, b, fraction) = node, let sv = view as? NSSplitView, sv.arrangedSubviews.count == 2 else { return }
        let total = horizontal ? sv.bounds.width : sv.bounds.height
        if total > 1 { sv.setPosition(total * fraction, ofDividerAt: 0) }
        applyFractions(a, in: sv.arrangedSubviews[0])
        applyFractions(b, in: sv.arrangedSubviews[1])
    }

    // MARK: NSSplitViewDelegate

    func splitViewDidResizeSubviews(_ notification: Notification) {
        guard !applyingLayout,
              let sv = notification.object as? NSSplitView,
              let pair = splits[ObjectIdentifier(sv)],
              sv.arrangedSubviews.count == 2 else { return }
        let first = sv.arrangedSubviews[0].frame
        let total = sv.isVertical ? sv.bounds.width : sv.bounds.height
        guard total > 1 else { return }
        let fraction = Double((sv.isVertical ? first.width : first.height) / total)
        onFractionChanged?(fraction, pair.first, pair.second)
    }
}

import AppKit

/// The tab strip above the pane area. Quiet by design — Presence spends color
/// on state, so a tab is text, and the only mark is the thin teal underline on
/// the active one. Hidden entirely while there is a single tab: an app that
/// shows a one-tab tab bar is an app charging you chrome for nothing.
@MainActor
final class SurfaceTabBar: NSView {
    var onSelect: ((Int) -> Void)?
    var onClose: ((Int) -> Void)?
    var onNew: (() -> Void)?

    private let row = NSStackView()

    init() {
        super.init(frame: .zero)
        row.orientation = .horizontal
        row.spacing = 2
        row.alignment = .centerY
        row.edgeInsets = NSEdgeInsets(top: 0, left: 8, bottom: 0, right: 8)
        row.translatesAutoresizingMaskIntoConstraints = false
        addSubview(row)
        NSLayoutConstraint.activate([
            row.leadingAnchor.constraint(equalTo: leadingAnchor), row.trailingAnchor.constraint(lessThanOrEqualTo: trailingAnchor),
            row.topAnchor.constraint(equalTo: topAnchor), row.bottomAnchor.constraint(equalTo: bottomAnchor),
            heightAnchor.constraint(equalToConstant: 26),
        ])
        // A plain NSView is not an accessibility element by default, so without
        // this the strip has an identifier nothing can look up.
        setAccessibilityElement(true)
        setAccessibilityIdentifier("center.tabbar")
        setAccessibilityRole(.tabGroup)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { nil }

    /// One tab's views, kept so an update can restyle instead of rebuild.
    private struct Cell {
        let view: NSView
        let button: NSButton
        let underline: NSView
    }

    private var cells: [Cell] = []
    private var shown: (titles: [String], active: Int) = ([], -1)

    /// How an update reaches the strip (SMOODEV-4127): nothing when nothing
    /// changed (every render calls this), in place when only titles or the
    /// active tab did, a rebuild only when tabs came or went.
    enum Change: Equatable { case none, restyle, rebuild }

    static func change(from old: (titles: [String], active: Int), to new: (titles: [String], active: Int)) -> Change {
        if old.titles == new.titles, old.active == new.active { return .none }
        return old.titles.count == new.titles.count ? .restyle : .rebuild
    }

    func update(titles: [String], active: Int) {
        let change = Self.change(from: shown, to: (titles, active))
        shown = (titles, active)
        switch change {
        case .none:
            return
        case .restyle where cells.count == titles.count:
            for (i, title) in titles.enumerated() { style(cells[i], title: title, active: i == active) }
            return
        case .restyle where titles.count < 2:
            return // a lone tab has no strip to restyle
        case .restyle, .rebuild:
            break
        }
        row.arrangedSubviews.forEach { $0.removeFromSuperview() }
        cells = []
        isHidden = titles.count < 2
        guard titles.count > 1 else { return }
        for (i, title) in titles.enumerated() {
            let c = tab(title, index: i, active: i == active)
            cells.append(c)
            row.addArrangedSubview(c.view)
        }
        let plus = NSButton(title: "+", target: self, action: #selector(newTab))
        plus.bezelStyle = .accessoryBarAction
        plus.controlSize = .small
        plus.setAccessibilityIdentifier("center.tab.new")
        plus.toolTip = "New Tab"
        row.addArrangedSubview(plus)
    }

    /// Tab `index`'s cell, while the strip shows — where ⌘⇧R's popover points.
    func cell(at index: Int) -> NSView? {
        guard !isHidden, cells.indices.contains(index) else { return nil }
        return cells[index].view
    }

    private func style(_ c: Cell, title: String, active: Bool) {
        if c.button.title != title { c.button.title = title }
        c.button.font = .systemFont(ofSize: 11, weight: active ? .semibold : .regular)
        c.button.contentTintColor = active ? nil : Theme.muted
        c.underline.layer?.backgroundColor = active ? Theme.teal.cgColor : NSColor.clear.cgColor
    }

    private func tab(_ title: String, index: Int, active: Bool) -> Cell {
        let button = NSButton(title: title, target: self, action: #selector(select(_:)))
        button.tag = index
        button.bezelStyle = .accessoryBarAction
        button.isBordered = false
        button.controlSize = .small
        button.setAccessibilityIdentifier("center.tab.\(index)")

        let close = NSButton(title: "×", target: self, action: #selector(close(_:)))
        close.tag = index
        close.isBordered = false
        close.controlSize = .small
        close.contentTintColor = Theme.faint
        close.setAccessibilityIdentifier("center.tab.close.\(index)")
        close.toolTip = "Close Tab"

        let underline = NSView()
        underline.wantsLayer = true
        underline.translatesAutoresizingMaskIntoConstraints = false

        let labels = NSStackView(views: [button, close])
        labels.orientation = .horizontal
        labels.spacing = 0
        let cell = NSStackView(views: [labels, underline])
        cell.orientation = .vertical
        cell.spacing = 2
        NSLayoutConstraint.activate([underline.heightAnchor.constraint(equalToConstant: 2), underline.widthAnchor.constraint(equalTo: labels.widthAnchor)])
        let c = Cell(view: cell, button: button, underline: underline)
        style(c, title: title, active: active)
        return c
    }

    /// Which tab a point in the strip falls on: the cell whose span covers
    /// `x`, so the gap between two tabs closes neither.
    static func tabIndex(atX x: CGFloat, spans: [ClosedRange<CGFloat>]) -> Int? {
        spans.firstIndex { $0.contains(x) }
    }

    /// Middle-click closes a tab, as in every browser and terminal. The
    /// buttons do not take the other mouse button, so it reaches the strip.
    override func otherMouseUp(with event: NSEvent) {
        guard event.buttonNumber == 2, !isHidden else { return super.otherMouseUp(with: event) }
        let x = convert(event.locationInWindow, from: nil).x
        let spans = cells.map { c -> ClosedRange<CGFloat> in
            let f = c.view.convert(c.view.bounds, to: self)
            return f.minX...f.maxX
        }
        if let i = Self.tabIndex(atX: x, spans: spans) { onClose?(i) }
    }

    @objc private func select(_ sender: NSButton) { onSelect?(sender.tag) }
    @objc private func close(_ sender: NSButton) { onClose?(sender.tag) }
    @objc private func newTab() { onNew?() }
}

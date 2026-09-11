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

    func update(titles: [String], active: Int) {
        row.arrangedSubviews.forEach { $0.removeFromSuperview() }
        isHidden = titles.count < 2
        guard titles.count > 1 else { return }
        for (i, title) in titles.enumerated() {
            row.addArrangedSubview(tab(title, index: i, active: i == active))
        }
        let plus = NSButton(title: "+", target: self, action: #selector(newTab))
        plus.bezelStyle = .accessoryBarAction
        plus.controlSize = .small
        plus.setAccessibilityIdentifier("center.tab.new")
        plus.toolTip = "New Tab"
        row.addArrangedSubview(plus)
    }

    private func tab(_ title: String, index: Int, active: Bool) -> NSView {
        let button = NSButton(title: title, target: self, action: #selector(select(_:)))
        button.tag = index
        button.bezelStyle = .accessoryBarAction
        button.isBordered = false
        button.controlSize = .small
        button.font = .systemFont(ofSize: 11, weight: active ? .semibold : .regular)
        button.contentTintColor = active ? nil : Theme.muted
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
        underline.layer?.backgroundColor = active ? Theme.teal.cgColor : NSColor.clear.cgColor
        underline.translatesAutoresizingMaskIntoConstraints = false

        let labels = NSStackView(views: [button, close])
        labels.orientation = .horizontal
        labels.spacing = 0
        let cell = NSStackView(views: [labels, underline])
        cell.orientation = .vertical
        cell.spacing = 2
        NSLayoutConstraint.activate([underline.heightAnchor.constraint(equalToConstant: 2), underline.widthAnchor.constraint(equalTo: labels.widthAnchor)])
        return cell
    }

    @objc private func select(_ sender: NSButton) { onSelect?(sender.tag) }
    @objc private func close(_ sender: NSButton) { onClose?(sender.tag) }
    @objc private func newTab() { onNew?() }
}

import AppKit

/// What the find bar shows (SMOODEV-4266). libghostty does the searching: the
/// app sends `search:<needle>` and `navigate_search:next|previous` binding
/// actions, and the counts come back as `SEARCH_TOTAL` / `SEARCH_SELECTED`.
struct TerminalFindState: Equatable {
    var needle = ""
    /// Matches so far; nil until libghostty has reported.
    var total: Int?
    /// The selected match, 0-based; nil when none is selected.
    var selected: Int?

    var countLabel: String {
        guard !needle.isEmpty else { return "" }
        switch (selected, total) {
        case (_, 0?): return "No matches"
        case let (s?, t?): return "\(s + 1) of \(t)"
        case let (nil, t?): return "\(t) found"
        default: return "…"
        }
    }

    /// libghostty reports "none" as a negative count.
    static func reported(_ raw: Int) -> Int? { raw >= 0 ? raw : nil }

    /// One- and two-letter needles match most of a long scrollback, so they
    /// wait for the next keystroke. Anything longer searches at once.
    static func debounce(forNeedle needle: String) -> TimeInterval {
        (1...2).contains(needle.count) ? 0.25 : 0
    }

    /// The binding action for `needle`. An empty one ends the search.
    static func searchAction(_ needle: String) -> String { "search:\(needle)" }
    static func navigateAction(forward: Bool) -> String { "navigate_search:\(forward ? "next" : "previous")" }
}

/// The find bar pinned to a surface's top-right corner. It is a subview of the
/// surface, so the search stays with the session when a pane or tab moves it.
@MainActor
final class TerminalFindBar: NSView, NSTextFieldDelegate {
    var onNeedle: ((String) -> Void)?
    var onNavigate: ((Bool) -> Void)?
    var onClose: (() -> Void)?
    var onFocus: (() -> Void)?

    let field = NSTextField()
    let count = NSTextField(labelWithString: "")

    init() {
        super.init(frame: .zero)
        wantsLayer = true
        layer?.backgroundColor = NSColor.windowBackgroundColor.cgColor
        layer?.borderColor = NSColor.separatorColor.cgColor
        layer?.borderWidth = 1
        layer?.cornerRadius = 8
        setAccessibilityIdentifier("find.bar")

        field.placeholderString = "Find"
        field.font = Theme.monoNSFont(size: 12)
        field.isBordered = false
        field.drawsBackground = false
        field.focusRingType = .none
        field.lineBreakMode = .byClipping
        field.usesSingleLineMode = true
        field.delegate = self
        field.setAccessibilityIdentifier("find.field")

        count.font = .monospacedDigitSystemFont(ofSize: 11, weight: .regular)
        count.textColor = Theme.muted
        count.alignment = .right
        count.setAccessibilityIdentifier("find.count")
        count.setContentHuggingPriority(.required, for: .horizontal)
        count.setContentCompressionResistancePriority(.required, for: .horizontal)

        let previous = button("chevron.up", "Find Previous", "find.previous", #selector(goPrevious))
        let next = button("chevron.down", "Find Next", "find.next", #selector(goNext))
        let close = button("xmark", "Close Find", "find.close", #selector(closeBar))

        let row = NSStackView(views: [field, count, previous, next, close])
        row.orientation = .horizontal
        row.spacing = 6
        row.translatesAutoresizingMaskIntoConstraints = false
        addSubview(row)
        NSLayoutConstraint.activate([
            row.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 10),
            row.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -6),
            row.centerYAnchor.constraint(equalTo: centerYAnchor),
            heightAnchor.constraint(equalToConstant: 30),
            widthAnchor.constraint(equalToConstant: 320),
        ])
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { nil }

    private func button(_ symbol: String, _ label: String, _ id: String, _ action: Selector) -> NSButton {
        let image = NSImage(systemSymbolName: symbol, accessibilityDescription: label) ?? NSImage()
        let b = NSButton(image: image, target: self, action: action)
        b.isBordered = false
        b.toolTip = label
        b.contentTintColor = Theme.muted
        b.setAccessibilityIdentifier(id)
        b.widthAnchor.constraint(equalToConstant: 20).isActive = true
        return b
    }

    @objc private func goPrevious() { onNavigate?(false) }
    @objc private func goNext() { onNavigate?(true) }
    @objc private func closeBar() { onClose?() }

    func show(_ state: TerminalFindState) {
        field.stringValue = state.needle
        count.stringValue = state.countLabel
    }

    func focusField() {
        window?.makeFirstResponder(field)
        field.currentEditor()?.selectAll(nil)
    }

    func controlTextDidBeginEditing(_ obj: Notification) { onFocus?() }
    func controlTextDidChange(_ obj: Notification) { onNeedle?(field.stringValue) }

    /// Return goes to the next match, ⇧Return to the previous, Esc closes.
    func control(_ control: NSControl, textView: NSTextView, doCommandBy selector: Selector) -> Bool {
        switch selector {
        case #selector(NSResponder.insertNewline(_:)), #selector(NSResponder.insertLineBreak(_:)), #selector(NSResponder.insertNewlineIgnoringFieldEditor(_:)):
            onNavigate?(!(NSApp.currentEvent?.modifierFlags.contains(.shift) ?? false))
            return true
        case #selector(NSResponder.cancelOperation(_:)):
            onClose?()
            return true
        default:
            return false
        }
    }
}

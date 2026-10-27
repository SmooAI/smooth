import AppKit

/// ⌘⇧R (SMOODEV-4127): name the focused pane's session, the way cmux names
/// a tab. The name lives on the session row (`flow.rename`), so the phones
/// and `th flow ls` show it too, and it is pinned: a shell's live name (the
/// program running in it, else its folder) no longer replaces it. Return
/// commits, Esc cancels, an empty name goes back to the automatic one.
enum TabRename {
    /// What a committed field asks for: a name, or nil to reset.
    static func requested(_ text: String) -> String? {
        let t = text.trimmingCharacters(in: .whitespacesAndNewlines)
        return t.isEmpty ? nil : t
    }
}

/// The small popover the rename happens in, anchored to the tab (or, with
/// the tab strip hidden, the pane's header).
@MainActor
final class TabRenamePopover: NSObject, NSTextFieldDelegate, NSPopoverDelegate {
    private let popover = NSPopover()
    private let field = NSTextField()
    private var commit: ((String?) -> Void)?

    override init() {
        super.init()
        field.placeholderString = "Automatic name"
        field.font = .systemFont(ofSize: 12)
        field.focusRingType = .none
        field.delegate = self
        field.setAccessibilityIdentifier("tab.rename.field")
        field.translatesAutoresizingMaskIntoConstraints = false
        let hint = NSTextField(labelWithString: "Empty goes back to the automatic name")
        hint.font = .systemFont(ofSize: 10)
        hint.textColor = Theme.muted
        hint.translatesAutoresizingMaskIntoConstraints = false
        let box = NSView()
        box.addSubview(field)
        box.addSubview(hint)
        NSLayoutConstraint.activate([
            field.leadingAnchor.constraint(equalTo: box.leadingAnchor, constant: 10),
            field.trailingAnchor.constraint(equalTo: box.trailingAnchor, constant: -10),
            field.topAnchor.constraint(equalTo: box.topAnchor, constant: 10),
            field.widthAnchor.constraint(equalToConstant: 240),
            hint.leadingAnchor.constraint(equalTo: field.leadingAnchor),
            hint.topAnchor.constraint(equalTo: field.bottomAnchor, constant: 4),
            hint.bottomAnchor.constraint(equalTo: box.bottomAnchor, constant: -8),
        ])
        let vc = NSViewController()
        vc.view = box
        popover.contentViewController = vc
        popover.behavior = .transient
        popover.delegate = self
    }

    var isShown: Bool { popover.isShown }

    /// Open over `anchor` with `current` selected; `commit` gets the new name,
    /// or nil to reset. Not called on Esc or a click away.
    func show(current: String, over anchor: NSView, commit: @escaping (String?) -> Void) {
        self.commit = commit
        field.stringValue = current
        popover.show(relativeTo: anchor.bounds, of: anchor, preferredEdge: .maxY)
        popover.contentViewController?.view.window?.makeFirstResponder(field)
        field.currentEditor()?.selectAll(nil)
    }

    func control(_ control: NSControl, textView: NSTextView, doCommandBy selector: Selector) -> Bool {
        switch selector {
        case #selector(NSResponder.insertNewline(_:)):
            let done = commit
            commit = nil
            popover.performClose(nil)
            done?(TabRename.requested(field.stringValue))
            return true
        case #selector(NSResponder.cancelOperation(_:)):
            commit = nil
            popover.performClose(nil)
            return true
        default:
            return false
        }
    }

    func popoverDidClose(_ notification: Notification) { commit = nil }
}

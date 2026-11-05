import AppKit
import SwiftUI

/// One row of the command palette (⌘⇧P, SMOODEV-4266).
struct PaletteItem: Identifiable, Equatable {
    enum Target: Equatable {
        case action(FlowAction)
        case session(String)
    }

    let target: Target
    let title: String
    let detail: String
    var chord: String?
    /// A session row's state dot.
    var session: Session?

    var id: String {
        switch target {
        case let .action(a): "action.\(a.rawValue)"
        case let .session(s): "session.\(s)"
        }
    }
}

/// The palette's rules, separate from its window so they can be tested.
/// Actions come from `FlowAction`, the keymap's registry, so a new action and a
/// rebound chord both show up here without this file changing.
enum CommandPalette {
    /// Left out: view-scoped diff keys (they only work inside the diff),
    /// aliases (the same action again), Focus Session N (the sessions are
    /// listed by name) and the palette itself.
    static func listed(_ a: FlowAction) -> Bool {
        !a.isViewScoped && !a.isAlias && FlowAction.focusSessionIndex(a) == nil && a != .commandPalette
    }

    static func actions(chord: (FlowAction) -> KeyChord?) -> [PaletteItem] {
        FlowAction.allCases.filter(listed).map { a in
            PaletteItem(target: .action(a), title: a.title.replacingOccurrences(of: "…", with: ""), detail: a.category.rawValue, chord: chord(a)?.display)
        }
    }

    static func sessions(_ sessions: [Session]) -> [PaletteItem] {
        sessions.map { s in
            PaletteItem(target: .session(s.id), title: s.label, detail: [s.project, Theme.stateLabel(s)].filter { !$0.isEmpty }.joined(separator: " · "), session: s)
        }
    }

    /// How well `token` matches `text` as an in-order subsequence, or nil.
    /// Prefix beats word start beats contiguous beats scattered.
    static func score(_ token: String, in text: String) -> Int? {
        let needle = Array(token.lowercased())
        let hay = Array(text.lowercased())
        guard !needle.isEmpty, needle.count <= hay.count else { return needle.isEmpty ? 0 : nil }
        var total = 0
        var at = 0
        var last = -2
        for c in needle {
            guard let found = hay[at...].firstIndex(of: c) else { return nil }
            total += 1
            if found == 0 {
                total += 8
            } else if !hay[found - 1].isLetter, !hay[found - 1].isNumber {
                total += 6
            }
            if found == last + 1 { total += 4 }
            last = found
            at = found + 1
        }
        return total - hay.count / 16
    }

    /// Every whitespace-separated token must match the title or the detail. A
    /// title match outranks a detail match, and ties keep the given order.
    static func filter(_ items: [PaletteItem], query: String) -> [PaletteItem] {
        let tokens = query.split(whereSeparator: \.isWhitespace).map(String.init)
        guard !tokens.isEmpty else { return items }
        let scored = items.enumerated().compactMap { i, item -> (Int, Int, PaletteItem)? in
            var sum = 0
            for t in tokens {
                let title = score(t, in: item.title).map { $0 * 2 }
                let detail = score(t, in: item.detail)
                guard let best = [title, detail].compactMap({ $0 }).max() else { return nil }
                sum += best
            }
            return (sum, i, item)
        }
        return scored.sorted { $0.0 == $1.0 ? $0.1 < $1.1 : $0.0 > $1.0 }.map(\.2)
    }
}

@MainActor
final class CommandPaletteModel: ObservableObject {
    @Published private(set) var results: [PaletteItem]
    @Published private(set) var selected = 0
    private let items: [PaletteItem]

    init(items: [PaletteItem]) {
        self.items = items
        results = items
    }

    func setQuery(_ query: String) {
        results = CommandPalette.filter(items, query: query)
        selected = 0
    }

    /// Up and down wrap at the ends.
    func move(by delta: Int) {
        guard !results.isEmpty else { return }
        selected = ((selected + delta) % results.count + results.count) % results.count
    }

    func select(_ item: PaletteItem) {
        if let i = results.firstIndex(of: item) { selected = i }
    }

    var current: PaletteItem? { results.indices.contains(selected) ? results[selected] : nil }
}

private struct CommandPaletteList: View {
    @ObservedObject var model: CommandPaletteModel
    let choose: (PaletteItem) -> Void

    var body: some View {
        if model.results.isEmpty {
            Text("No matches").font(.callout).foregroundStyle(Color(Theme.muted))
                .frame(maxWidth: .infinity, maxHeight: .infinity)
                .accessibilityIdentifier("palette.empty")
        } else {
            ScrollViewReader { proxy in
                ScrollView {
                    LazyVStack(spacing: 0) {
                        ForEach(model.results) { item in row(item).id(item.id) }
                    }
                    .padding(6)
                }
                .onChange(of: model.selected) { _, _ in
                    if let id = model.current?.id { proxy.scrollTo(id) }
                }
            }
        }
    }

    private func row(_ item: PaletteItem) -> some View {
        let isSelected = model.current == item
        return HStack(spacing: 8) {
            Group {
                if let s = item.session { StateDot(session: s) } else { Color.clear }
            }
            .frame(width: 9, height: 9)
            Text(item.title).lineLimit(1).fontWeight(isSelected ? .semibold : .regular)
            Text(item.detail).font(.caption).foregroundStyle(Color(Theme.muted)).lineLimit(1)
            Spacer(minLength: 8)
            if let chord = item.chord {
                Text(chord).font(Theme.mono(.caption)).foregroundStyle(Color(Theme.muted))
            }
        }
        .padding(.horizontal, 8)
        .frame(height: 28)
        .background(isSelected ? Color.primary.opacity(0.09) : Color.clear, in: RoundedRectangle(cornerRadius: 6))
        .contentShape(Rectangle())
        .onTapGesture { choose(item) }
        .accessibilityElement(children: .combine)
        .accessibilityAddTraits(.isButton)
        .accessibilityIdentifier("palette.row.\(item.id)")
    }
}

/// A borderless panel can only take the keyboard if it says so.
private final class CommandPalettePanel: NSPanel {
    override var canBecomeKey: Bool { true }
}

/// The palette's window: a field over a list, floating at the top of the main
/// window. It goes away on Esc, on a choice and when it loses the keyboard.
@MainActor
final class CommandPaletteController: NSObject, NSTextFieldDelegate, NSWindowDelegate {
    private var panel: NSPanel?
    private(set) var model: CommandPaletteModel?
    private var run: ((PaletteItem.Target) -> Void)?
    private(set) var field: NSTextField?

    static let size = NSSize(width: 560, height: 380)

    var isShown: Bool { panel != nil }

    func toggle(over window: NSWindow?, items: [PaletteItem], run: @escaping (PaletteItem.Target) -> Void) {
        if isShown {
            close()
        } else {
            show(over: window, items: items, run: run)
        }
    }

    func show(over window: NSWindow?, items: [PaletteItem], run: @escaping (PaletteItem.Target) -> Void) {
        close()
        let model = CommandPaletteModel(items: items)
        self.model = model
        self.run = run

        let field = NSTextField()
        field.placeholderString = "Search actions and sessions"
        field.font = .systemFont(ofSize: 15)
        field.isBordered = false
        field.drawsBackground = false
        field.focusRingType = .none
        field.usesSingleLineMode = true
        field.delegate = self
        field.setAccessibilityIdentifier("palette.field")
        field.translatesAutoresizingMaskIntoConstraints = false
        self.field = field

        let rule = NSBox()
        rule.boxType = .separator
        rule.translatesAutoresizingMaskIntoConstraints = false
        let list = NSHostingView(rootView: CommandPaletteList(model: model) { [weak self] in self?.choose($0) })
        list.translatesAutoresizingMaskIntoConstraints = false

        let content = NSView(frame: NSRect(origin: .zero, size: Self.size))
        content.wantsLayer = true
        content.layer?.backgroundColor = NSColor.windowBackgroundColor.cgColor
        content.layer?.borderColor = NSColor.separatorColor.cgColor
        content.layer?.borderWidth = 1
        content.layer?.cornerRadius = 10
        content.layer?.masksToBounds = true
        [field, rule, list].forEach(content.addSubview)
        NSLayoutConstraint.activate([
            field.leadingAnchor.constraint(equalTo: content.leadingAnchor, constant: 16),
            field.trailingAnchor.constraint(equalTo: content.trailingAnchor, constant: -16),
            field.topAnchor.constraint(equalTo: content.topAnchor, constant: 12),
            rule.leadingAnchor.constraint(equalTo: content.leadingAnchor),
            rule.trailingAnchor.constraint(equalTo: content.trailingAnchor),
            rule.topAnchor.constraint(equalTo: field.bottomAnchor, constant: 10),
            list.leadingAnchor.constraint(equalTo: content.leadingAnchor),
            list.trailingAnchor.constraint(equalTo: content.trailingAnchor),
            list.topAnchor.constraint(equalTo: rule.bottomAnchor),
            list.bottomAnchor.constraint(equalTo: content.bottomAnchor),
        ])

        let panel = CommandPalettePanel(contentRect: NSRect(origin: .zero, size: Self.size), styleMask: [.borderless, .nonactivatingPanel], backing: .buffered, defer: false)
        panel.contentView = content
        panel.isOpaque = false
        panel.backgroundColor = .clear
        panel.hasShadow = true
        panel.isReleasedWhenClosed = false
        panel.delegate = self
        panel.setAccessibilityIdentifier("palette")
        if let window {
            panel.setFrameOrigin(Self.origin(in: window.frame, size: Self.size))
            window.addChildWindow(panel, ordered: .above)
        } else {
            panel.center()
        }
        self.panel = panel
        panel.makeKeyAndOrderFront(nil)
        panel.makeFirstResponder(field)
    }

    /// Centered, a little below the window's title bar.
    static func origin(in frame: NSRect, size: NSSize) -> NSPoint {
        NSPoint(x: frame.midX - size.width / 2, y: frame.maxY - size.height - 72)
    }

    func close() {
        guard let panel else { return }
        self.panel = nil
        panel.delegate = nil
        panel.parent?.removeChildWindow(panel)
        panel.orderOut(nil)
        model = nil
        field = nil
        run = nil
    }

    func choose(_ item: PaletteItem) {
        let run = run
        close()
        run?(item.target)
    }

    func windowDidResignKey(_ notification: Notification) { close() }

    func controlTextDidChange(_ obj: Notification) { model?.setQuery(field?.stringValue ?? "") }

    func control(_ control: NSControl, textView: NSTextView, doCommandBy selector: Selector) -> Bool {
        switch selector {
        case #selector(NSResponder.moveDown(_:)):
            model?.move(by: 1)
        case #selector(NSResponder.moveUp(_:)):
            model?.move(by: -1)
        case #selector(NSResponder.insertNewline(_:)):
            if let item = model?.current { choose(item) }
        case #selector(NSResponder.cancelOperation(_:)):
            close()
        default:
            return false
        }
        return true
    }
}

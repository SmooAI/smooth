import AppKit
import SwiftUI

/// Settings ▸ Keyboard: every action, the chord that fires it, and a recorder
/// to change it. The pane and `~/.smooth/smoothflow/keybindings.toml` are the
/// same store — this writes that file, and "Reveal" opens it — so neither
/// surface can be the stale one.
struct KeyboardPane: View {
    @ObservedObject var keymap: KeymapStore
    @State private var recording: FlowAction?
    @State private var query = ""

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("Click a shortcut, then press the keys you want. ⎋ cancels, ⌫ unbinds. Changes are written to ~/.smooth/smoothflow/keybindings.toml, which you can also edit by hand.")
                .font(.caption).foregroundStyle(Color(Theme.muted))
            HStack {
                TextField("Filter", text: $query).textFieldStyle(.roundedBorder).accessibilityIdentifier("settings.keyboard.filter")
                Button("Reveal file") { revealFile() }.controlSize(.small)
                Button("Reload") { keymap.reload() }.controlSize(.small)
                Button("Reset all") { keymap.resetAll() }.controlSize(.small)
                    .disabled(keymap.map.overrides.isEmpty)
                    .accessibilityIdentifier("settings.keyboard.resetAll")
            }
            if !keymap.map.problems.isEmpty {
                VStack(alignment: .leading, spacing: 2) {
                    ForEach(keymap.map.problems, id: \.self) { Text($0).font(Theme.mono(.caption)).foregroundStyle(Color(Theme.amber)) }
                }
                .accessibilityIdentifier("settings.keyboard.problems")
            }
            ScrollView {
                // Not a LazyVStack: it materializes only the visible rows, so a
                // row below the fold has no accessibility element — VoiceOver
                // and the UI test both lose it. Forty rows cost nothing.
                VStack(alignment: .leading, spacing: 0) {
                    ForEach(FlowAction.Category.allCases) { category in
                        let rows = actions(in: category)
                        if !rows.isEmpty {
                            Text(category.rawValue.uppercased())
                                .font(.caption.bold()).foregroundStyle(Color(Theme.muted))
                                .padding(.top, 10).padding(.bottom, 4)
                            ForEach(rows) { row($0) }
                        }
                    }
                }
            }
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("settings.pane.keyboard")
    }

    private func actions(in category: FlowAction.Category) -> [FlowAction] {
        FlowAction.allCases.filter { a in
            guard a.category == category else { return false }
            guard !query.isEmpty else { return true }
            let needle = query.lowercased()
            return a.title.lowercased().contains(needle)
                || a.rawValue.lowercased().contains(needle)
                || (keymap.chord(for: a)?.wire.contains(needle) ?? false)
        }
    }

    private func row(_ action: FlowAction) -> some View {
        let chord = keymap.chord(for: action)
        let partners = keymap.map.conflictPartners(of: action)
        return HStack(alignment: .firstTextBaseline, spacing: 8) {
            VStack(alignment: .leading, spacing: 1) {
                Text(action.title).font(.body)
                if let note = action.note {
                    Text(note).font(.caption).foregroundStyle(Color(Theme.muted))
                }
                if !partners.isEmpty {
                    Text("Same shortcut as \(partners.map(\.title).joined(separator: ", ")) — the first one in the menu bar wins.")
                        .font(.caption).foregroundStyle(Color(Theme.amber))
                        .accessibilityIdentifier("settings.keyboard.conflict.\(action.rawValue)")
                }
            }
            Spacer(minLength: 12)
            ChordButton(label: recording == action ? "Press keys…" : (chord?.display ?? "—"),
                        recording: recording == action,
                        conflicted: !partners.isEmpty,
                        identifier: "settings.keyboard.chord.\(action.rawValue)") {
                recording = recording == action ? nil : action
            } onChord: { pressed in
                guard recording == action else { return false }
                apply(pressed, to: action)
                return true
            }
            Button("↺") { keymap.reset(action) }
                .controlSize(.small)
                .disabled(!keymap.map.isCustom(action))
                .help("Reset to default (\(action.defaultChord?.display ?? "none"))")
                .accessibilityIdentifier("settings.keyboard.reset.\(action.rawValue)")
        }
        .padding(.vertical, 3)
    }

    /// `nil` means the recorder saw ⎋ (cancel) or ⌫ (unbind) — the two keys a
    /// recorder must never take literally, or you can bind a shortcut and then
    /// never reach the control that would undo it.
    private func apply(_ pressed: KeyChord?, to action: FlowAction) {
        defer { recording = nil }
        guard let pressed else { return }
        if pressed.key == "escape", !pressed.hasModifier { return }
        if pressed.key == "backspace", !pressed.hasModifier { keymap.set(action, to: nil); return }
        guard pressed.hasModifier else { return }
        keymap.set(action, to: pressed)
    }

    private func revealFile() {
        try? FileManager.default.createDirectory(at: Keymap.directory, withIntermediateDirectories: true)
        if !FileManager.default.fileExists(atPath: Keymap.fileURL.path) {
            try? keymap.map.save(to: Keymap.fileURL)
        }
        NSWorkspace.shared.activateFileViewerSelecting([Keymap.fileURL])
    }
}

/// The recorder button: click it, and the next keypress becomes the binding.
/// A plain SwiftUI button cannot do this — it never sees ⌘-anything, because
/// AppKit hands ⌘ chords to the menu bar first — so this is an NSView that
/// takes first responder and reads `keyDown` itself.
struct ChordButton: NSViewRepresentable {
    var label: String
    var recording: Bool
    var conflicted: Bool
    var identifier: String
    var onClick: () -> Void
    /// Returns true when the chord was consumed.
    var onChord: (KeyChord?) -> Bool

    func makeNSView(context: Context) -> ChordRecorderView {
        let v = ChordRecorderView()
        v.setAccessibilityIdentifier(identifier)
        return v
    }

    func updateNSView(_ v: ChordRecorderView, context: Context) {
        v.onClick = onClick
        v.onChord = onChord
        v.configure(label: label, recording: recording, conflicted: conflicted)
    }
}

@MainActor
final class ChordRecorderView: NSView {
    var onClick: (() -> Void)?
    var onChord: ((KeyChord?) -> Bool)?
    private let field = NSTextField(labelWithString: "")
    private var recording = false

    override init(frame: NSRect) {
        super.init(frame: frame)
        // A plain NSView is not an accessibility element, and setting a role
        // does not make it one — without this the recorder has an identifier
        // that neither VoiceOver nor XCUITest can find.
        setAccessibilityElement(true)
        wantsLayer = true
        layer?.cornerRadius = 5
        layer?.borderWidth = 1
        field.alignment = .center
        field.translatesAutoresizingMaskIntoConstraints = false
        addSubview(field)
        NSLayoutConstraint.activate([
            field.centerXAnchor.constraint(equalTo: centerXAnchor), field.centerYAnchor.constraint(equalTo: centerYAnchor),
            widthAnchor.constraint(greaterThanOrEqualToConstant: 92), heightAnchor.constraint(equalToConstant: 22),
            field.leadingAnchor.constraint(greaterThanOrEqualTo: leadingAnchor, constant: 8),
        ])
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { nil }

    func configure(label: String, recording: Bool, conflicted: Bool) {
        field.stringValue = label
        // The chord is the control's value, not a child text field: a UI test
        // (and VoiceOver) must be able to read the binding off the row.
        setAccessibilityRole(.button)
        setAccessibilityLabel(label)
        setAccessibilityValue(label)
        field.font = Theme.monoNSFont(size: 12)
        field.textColor = conflicted ? Theme.amber : (recording ? Theme.teal : .labelColor)
        layer?.borderColor = (recording ? Theme.teal : Theme.faint.withAlphaComponent(0.5)).cgColor
        layer?.backgroundColor = (recording ? Theme.teal.withAlphaComponent(0.08) : .clear).cgColor
        if self.recording != recording {
            self.recording = recording
            if recording { window?.makeFirstResponder(self) } else if window?.firstResponder === self { window?.makeFirstResponder(nil) }
        }
    }

    override var acceptsFirstResponder: Bool { recording }
    override func mouseDown(with event: NSEvent) { onClick?() }

    override func keyDown(with event: NSEvent) {
        guard recording else { super.keyDown(with: event); return }
        if onChord?(KeyChord.from(event: event)) != true { super.keyDown(with: event) }
    }

    /// Menu shortcuts (⌘T, ⌘W, …) never reach `keyDown` — the menu bar eats
    /// them first — so the recorder has to claim them one layer earlier. This
    /// is why a recorder can capture a chord that is already bound.
    override func performKeyEquivalent(with event: NSEvent) -> Bool {
        guard recording else { return super.performKeyEquivalent(with: event) }
        return onChord?(KeyChord.from(event: event)) == true
    }
}

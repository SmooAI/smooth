import Foundation

/// The resolved keyboard map: every `FlowAction` and the chord that fires it.
///
/// Defaults come from `FlowAction.defaultChord`; a user file overrides them by
/// action name. The file is the source of truth for what the user changed —
/// Settings ▸ Keyboard writes the same file, so editing either surface is the
/// same act, and a text editor and the pane never disagree.
struct Keymap: Equatable, Sendable {
    /// Only what differs from the defaults. An explicit `nil` value means the
    /// user cleared a default binding, which is not the same as never having
    /// touched it.
    private(set) var overrides: [FlowAction: KeyChord?] = [:]

    /// Lines the file carried that we could not use — surfaced in the pane
    /// rather than swallowed, so a typo is visible instead of mysterious.
    private(set) var problems: [String] = []

    static let `default` = Keymap()

    init(overrides: [FlowAction: KeyChord?] = [:], problems: [String] = []) {
        self.overrides = overrides
        self.problems = problems
    }

    /// The chord that currently fires `action`, or nil when it has none.
    func chord(for action: FlowAction) -> KeyChord? {
        if let override = overrides[action] { return override }
        return action.defaultChord
    }

    func isCustom(_ action: FlowAction) -> Bool { overrides[action] != nil }

    /// Bind `action` to `chord` (nil unbinds it). Re-binding an action to its
    /// own default drops the override rather than storing a redundant line.
    mutating func set(_ action: FlowAction, to chord: KeyChord?) {
        if chord == action.defaultChord {
            overrides.removeValue(forKey: action)
        } else {
            overrides[action] = chord
        }
    }

    mutating func reset(_ action: FlowAction) { overrides.removeValue(forKey: action) }
    mutating func resetAll() { overrides = [:]; problems = [] }

    /// Actions sharing a chord, keyed by that chord. A conflict is reported,
    /// never auto-resolved: which of the two the user wanted is not ours to
    /// guess, and AppKit's own answer (first matching menu item wins) is at
    /// least stable.
    var conflicts: [KeyChord: [FlowAction]] {
        var byChord: [KeyChord: [FlowAction]] = [:]
        for action in FlowAction.allCases {
            guard let c = chord(for: action) else { continue }
            byChord[c, default: []].append(action)
        }
        return byChord.filter { $0.value.count > 1 }
    }

    func conflictPartners(of action: FlowAction) -> [FlowAction] {
        guard let c = chord(for: action) else { return [] }
        return conflicts[c]?.filter { $0 != action } ?? []
    }

    // MARK: file

    static var directory: URL {
        FileManager.default.homeDirectoryForCurrentUser.appendingPathComponent(".smooth/smoothflow", isDirectory: true)
    }

    static var fileURL: URL { directory.appendingPathComponent("keybindings.toml") }

    /// Read the file, or the defaults when it is absent. A file that parses to
    /// nothing usable still yields a working map: SmoothFlow is a terminal you
    /// steer agents from, and losing every shortcut to a stray bracket is not
    /// an acceptable failure mode. What went wrong rides along in `problems`.
    static func load(from url: URL = Keymap.fileURL) -> Keymap {
        guard let text = try? String(contentsOf: url, encoding: .utf8) else { return .default }
        return parse(text)
    }

    /// The `[keys]` table of the TOML file: `action = "chord"`, or
    /// `action = ""` to unbind. Unknown actions and unparseable chords become
    /// `problems`; everything else still loads.
    static func parse(_ text: String) -> Keymap {
        var map = Keymap()
        var inKeys = false
        for (i, rawLine) in text.components(separatedBy: .newlines).enumerated() {
            let line = TOML.stripComment(rawLine).trimmingCharacters(in: .whitespaces)
            if line.isEmpty { continue }
            if line.hasPrefix("[") {
                inKeys = line == "[keys]"
                continue
            }
            guard inKeys else { continue }
            guard let (name, value) = TOML.keyValue(line) else {
                map.problems.append("line \(i + 1): not `action = \"chord\"` — \(line)")
                continue
            }
            guard let action = FlowAction(rawValue: name) else {
                map.problems.append("line \(i + 1): unknown action `\(name)`")
                continue
            }
            if value.isEmpty {
                map.overrides[action] = KeyChord?.none
                continue
            }
            guard let chord = KeyChord.parse(value) else {
                map.problems.append("line \(i + 1): `\(value)` is not a shortcut (try `cmd+shift+d`)")
                continue
            }
            guard chord.hasModifier else {
                map.problems.append("line \(i + 1): `\(value)` has no modifier — a bare key would swallow terminal input")
                continue
            }
            map.overrides[action] = chord
        }
        // A default the user re-typed by hand is not an override.
        for (action, chord) in map.overrides where chord == action.defaultChord {
            map.overrides.removeValue(forKey: action)
        }
        return map
    }

    /// The file to write for this map — only the changed rows, so upgrading
    /// SmoothFlow's defaults still reaches anyone who did not opt out of them.
    var serialized: String {
        var out = """
        # SmoothFlow keybindings. Only what you changed belongs here — every
        # action not listed keeps the app's default, so new defaults still
        # reach you. Settings ▸ Keyboard (⌘,) writes this same file.
        #
        #   action = "cmd+shift+d"   rebind
        #   action = ""              unbind
        #
        # Modifiers: cmd, shift, opt, ctrl. Keys: a single character, or one of
        # enter, tab, space, escape, backspace, delete, left, right, up, down,
        # home, end, pageup, pagedown, f1-f20.

        [keys]

        """
        for action in FlowAction.allCases {
            guard let override = overrides[action] else { continue }
            out += "\(action.rawValue) = \"\(override?.wire ?? "")\"\n"
        }
        return out
    }

    func save(to url: URL = Keymap.fileURL) throws {
        try FileManager.default.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
        try serialized.write(to: url, atomically: true, encoding: .utf8)
    }
}

/// The sliver of TOML a keybinding file needs. Not a TOML library and not
/// pretending to be one: a `[table]` header and `key = "value"` lines.
enum TOML {
    /// Drop a trailing `#` comment, respecting a quoted `#`.
    static func stripComment(_ line: String) -> String {
        var inQuotes = false
        for (i, ch) in zip(line.indices, line) {
            if ch == "\"" { inQuotes.toggle() }
            if ch == "#", !inQuotes { return String(line[line.startIndex..<i]) }
        }
        return line
    }

    /// `key = "value"` → (key, value). Bare (unquoted) values are accepted too.
    static func keyValue(_ line: String) -> (String, String)? {
        guard let eq = line.firstIndex(of: "=") else { return nil }
        let key = line[line.startIndex..<eq].trimmingCharacters(in: .whitespaces)
        var value = line[line.index(after: eq)...].trimmingCharacters(in: .whitespaces)
        guard !key.isEmpty else { return nil }
        if value.count >= 2, value.hasPrefix("\""), value.hasSuffix("\"") {
            value = String(value.dropFirst().dropLast())
        } else if value.contains("\"") {
            return nil
        }
        return (key, value)
    }
}

/// The app's live keymap. One observable object so the menu bar and the
/// settings pane are never out of step: every write goes through here, saves
/// the file, and republishes.
@MainActor
final class KeymapStore: ObservableObject {
    @Published private(set) var map: Keymap
    private let url: URL
    /// Rebuild the menu bar. Set by the app delegate, which owns the menu.
    var onChange: (() -> Void)?

    init(url: URL = Keymap.fileURL) {
        self.url = url
        map = Keymap.load(from: url)
    }

    func chord(for action: FlowAction) -> KeyChord? { map.chord(for: action) }

    func set(_ action: FlowAction, to chord: KeyChord?) {
        map.set(action, to: chord)
        persist()
    }

    func reset(_ action: FlowAction) {
        map.reset(action)
        persist()
    }

    func resetAll() {
        map.resetAll()
        persist()
    }

    /// Re-read the file — for the user who edited it by hand while the app ran.
    func reload() {
        map = Keymap.load(from: url)
        onChange?()
    }

    private func persist() {
        do { try map.save(to: url) } catch { NSLog("SmoothFlow: could not write \(url.path): \(error.localizedDescription)") }
        onChange?()
    }
}

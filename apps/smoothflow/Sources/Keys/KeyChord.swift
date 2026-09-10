import AppKit

/// One keyboard shortcut: a key plus its modifiers.
///
/// The wire form is the one people already type in Ghostty / cmux / VS Code
/// keymaps — `cmd+shift+enter`, `cmd+opt+left`, `cmd+1` — because a keybinding
/// file nobody can read by eye is a file nobody edits. `⌘⇧↩` is the display
/// form and is never parsed.
struct KeyChord: Equatable, Hashable, Codable, Sendable {
    /// The key, normalized: a single lowercase character, or a named key
    /// (`enter`, `left`, `f1`, …). Never a modifier.
    var key: String
    var command = false
    var shift = false
    var option = false
    var control = false

    init(_ key: String, command: Bool = false, shift: Bool = false, option: Bool = false, control: Bool = false) {
        self.key = key
        self.command = command
        self.shift = shift
        self.option = option
        self.control = control
    }

    var hasModifier: Bool { command || shift || option || control }

    // MARK: named keys

    /// Named key → the character AppKit wants as a menu item's key equivalent.
    /// Only keys a menu can carry; a name outside this table is a plain
    /// character (`d`, `1`, `,`).
    static let namedEquivalents: [String: String] = [
        "enter": "\r", "return": "\r", "tab": "\t", "space": " ", "escape": "\u{1b}", "backspace": "\u{8}", "delete": "\u{7f}",
        "left": String(UnicodeScalar(NSLeftArrowFunctionKey)!), "right": String(UnicodeScalar(NSRightArrowFunctionKey)!),
        "up": String(UnicodeScalar(NSUpArrowFunctionKey)!), "down": String(UnicodeScalar(NSDownArrowFunctionKey)!),
        "home": String(UnicodeScalar(NSHomeFunctionKey)!), "end": String(UnicodeScalar(NSEndFunctionKey)!),
        "pageup": String(UnicodeScalar(NSPageUpFunctionKey)!), "pagedown": String(UnicodeScalar(NSPageDownFunctionKey)!),
    ]

    /// Aliases people type, folded onto the canonical name.
    static let keyAliases: [String: String] = [
        "return": "enter", "cr": "enter", "esc": "escape", "bs": "backspace", "del": "delete",
        "arrowleft": "left", "arrowright": "right", "arrowup": "up", "arrowdown": "down",
        "pgup": "pageup", "pgdn": "pagedown", "pagedn": "pagedown", "plus": "+", "minus": "-", "equal": "=",
    ]

    private static let modifierAliases: [String: WritableKeyPath<KeyChord, Bool>] = [
        "cmd": \.command, "command": \.command, "super": \.command, "meta": \.command,
        "shift": \.shift, "opt": \.option, "option": \.option, "alt": \.option,
        "ctrl": \.control, "control": \.control,
    ]

    // MARK: parsing

    /// `"cmd+shift+["` → a chord, or nil when the text names no key, names two,
    /// or uses a modifier we do not know. Case- and space-insensitive.
    ///
    /// `+` is both the separator and a legal key, so the last segment is taken
    /// literally: `cmd++` and `cmd+plus` both mean ⌘+.
    static func parse(_ raw: String) -> KeyChord? {
        let text = raw.trimmingCharacters(in: .whitespaces).lowercased()
        guard !text.isEmpty else { return nil }
        var parts = text.split(separator: "+", omittingEmptySubsequences: false).map(String.init)
        // A trailing empty segment is the literal `+` key ("cmd++" → ["cmd", "", ""]).
        if parts.count > 1, parts.last == "" {
            parts.removeLast()
            parts[parts.count - 1] = "+"
        }
        var chord = KeyChord("")
        var key: String?
        for part in parts {
            let p = part.trimmingCharacters(in: .whitespaces)
            guard !p.isEmpty else { return nil }
            if let path = modifierAliases[p] {
                guard !chord[keyPath: path] else { return nil }
                chord[keyPath: path] = true
                continue
            }
            guard key == nil else { return nil }
            key = keyAliases[p] ?? p
        }
        guard var k = key else { return nil }
        // Single characters only, beyond the named keys.
        if namedEquivalents[k] == nil, !isFunctionKeyName(k) {
            guard k.count == 1 else { return nil }
            k = k.lowercased()
        }
        chord.key = k
        return chord
    }

    static func isFunctionKeyName(_ k: String) -> Bool {
        guard k.hasPrefix("f"), let n = Int(k.dropFirst()) else { return false }
        return (1...20).contains(n)
    }

    /// The canonical wire form — what `format(parse(x))` settles on, and what
    /// the Keyboard pane writes back to the file.
    var wire: String {
        var out: [String] = []
        if control { out.append("ctrl") }
        if option { out.append("opt") }
        if shift { out.append("shift") }
        if command { out.append("cmd") }
        out.append(key)
        return out.joined(separator: "+")
    }

    /// `⌘⇧↩` — for menus and the settings table. Never round-trips.
    var display: String {
        var out = ""
        if control { out += "⌃" }
        if option { out += "⌥" }
        if shift { out += "⇧" }
        if command { out += "⌘" }
        return out + Self.glyph(key)
    }

    static func glyph(_ key: String) -> String {
        switch key {
        case "enter": "↩"
        case "tab": "⇥"
        case "space": "␣"
        case "escape": "⎋"
        case "backspace": "⌫"
        case "delete": "⌦"
        case "left": "←"
        case "right": "→"
        case "up": "↑"
        case "down": "↓"
        case "home": "↖"
        case "end": "↘"
        case "pageup": "⇞"
        case "pagedown": "⇟"
        default: key.count == 1 ? key.uppercased() : key.uppercased()
        }
    }

    // MARK: AppKit

    /// The `keyEquivalent` string for an `NSMenuItem`.
    ///
    /// Shift is carried in `keyEquivalentModifierMask`, NOT by uppercasing the
    /// character: AppKit treats an uppercase equivalent as implying shift, and
    /// setting both makes the item need ⇧⇧ — it simply never fires.
    var menuKeyEquivalent: String {
        if let named = Self.namedEquivalents[key] { return named }
        if Self.isFunctionKeyName(key), let n = Int(key.dropFirst()), let scalar = UnicodeScalar(NSF1FunctionKey + n - 1) { return String(scalar) }
        return key
    }

    var menuModifiers: NSEvent.ModifierFlags {
        var m: NSEvent.ModifierFlags = []
        if command { m.insert(.command) }
        if shift { m.insert(.shift) }
        if option { m.insert(.option) }
        if control { m.insert(.control) }
        return m
    }

    /// The chord an `NSEvent` represents — how the recorder in Settings ▸
    /// Keyboard reads a keypress. Returns nil for a modifier-only press.
    static func from(event: NSEvent) -> KeyChord? {
        let flags = event.modifierFlags
        // charactersIgnoringModifiers keeps ⌥ from turning `d` into `∂`, and
        // keeps the unshifted face of `[` rather than `{`.
        guard let raw = event.charactersIgnoringModifiers, let scalar = raw.unicodeScalars.first else { return nil }
        var key: String
        if let named = namedEquivalents.first(where: { $0.value.unicodeScalars.first == scalar && $0.key != "return" })?.key {
            key = named
        } else if scalar.value >= UInt32(NSF1FunctionKey), scalar.value <= UInt32(NSF1FunctionKey + 19) {
            key = "f\(Int(scalar.value) - NSF1FunctionKey + 1)"
        } else {
            key = String(scalar).lowercased()
        }
        key = keyAliases[key] ?? key
        guard !key.isEmpty, key != "\u{0}" else { return nil }
        return KeyChord(key,
                        command: flags.contains(.command),
                        shift: flags.contains(.shift),
                        option: flags.contains(.option),
                        control: flags.contains(.control))
    }
}

import AppKit
import CoreText
import SwiftUI

/// The terminal font, made deliberate (pearl th-bcd819).
///
/// libghostty compiles in JetBrains Mono plus a Symbols Nerd Font fallback, so
/// starship glyphs happened to render — but nothing named the font, and a user
/// `~/.config/ghostty` `font-family` silently decided what the panes looked
/// like. SmoothFlow now ships **JetBrainsMono Nerd Font** (OFL,
/// `Resources/Fonts`), registers it for this process at launch (no install),
/// and writes the ghostty font keys itself. Precedence, highest first:
///
/// 1. **Settings ▸ Terminal** — a family / size / ligature choice made in the
///    app (stored in UserDefaults, applied live to every open surface).
/// 2. **The user's Ghostty config** — a `font-family` (or `font-size`) in
///    `~/.config/ghostty/config` / the Application Support config is left
///    alone: their terminal, their font.
/// 3. **The bundled default** — `JetBrainsMono Nerd Font` at 13 pt.
///
/// The same family is used for the app's own monospace text (`Theme.mono`), so
/// chrome and panes match. Everything here is pure over explicit inputs except
/// `registerBundled` / `readUserConfigKeys`, which the tests drive directly.
enum TerminalFont {
    /// The family name inside the bundled TTFs (CoreText `kCTFontFamilyNameAttribute`).
    static let bundledFamily = "JetBrainsMono Nerd Font"
    /// The regular face's PostScript name — what `Font.custom` / `NSFont(name:)` want.
    static let bundledPostScriptRegular = "JetBrainsMonoNF-Regular"
    static let bundledFiles = ["JetBrainsMonoNerdFont-Regular", "JetBrainsMonoNerdFont-Bold", "JetBrainsMonoNerdFont-Italic", "JetBrainsMonoNerdFont-BoldItalic"]
    static let defaultSize: Double = 13
    static let sizeRange: ClosedRange<Double> = 8...32

    /// Set once `registerBundled` succeeded; nil ⇒ the system monospace font.
    private(set) static var registeredFamily: String?

    /// Register the bundled faces for THIS process (`.process` scope: nothing
    /// is installed for the user, nothing survives the app). Returns how many
    /// of the four faces are now usable; "already registered" counts.
    @discardableResult
    static func registerBundled(bundle: Bundle = .main) -> Int {
        var usable = 0
        for file in bundledFiles {
            guard let url = bundle.url(forResource: file, withExtension: "ttf") ?? bundle.url(forResource: file, withExtension: "ttf", subdirectory: "Fonts") else { continue }
            var error: Unmanaged<CFError>?
            if CTFontManagerRegisterFontsForURL(url as CFURL, .process, &error) {
                usable += 1
            } else if let e = error?.takeRetainedValue(), CFErrorGetCode(e) == CTFontManagerError.alreadyRegistered.rawValue {
                usable += 1
            }
        }
        if usable > 0 { registeredFamily = bundledFamily }
        return usable
    }

    // MARK: libghostty resources (themes)

    static let ghosttyAppResources = "/Applications/Ghostty.app/Contents/Resources/ghostty"

    /// Where libghostty should look for `themes/`: an explicit
    /// `GHOSTTY_RESOURCES_DIR` is respected (nil — nothing to set), then the
    /// bundled copy (`Contents/Resources/ghostty`), then an installed
    /// Ghostty.app's. Pure over its inputs.
    static func resourcesDir(env: [String: String] = ProcessInfo.processInfo.environment,
                             bundleResources: URL? = Bundle.main.resourceURL,
                             ghosttyApp: String = ghosttyAppResources,
                             exists: (String) -> Bool = { FileManager.default.fileExists(atPath: $0) }) -> String? {
        if let explicit = env["GHOSTTY_RESOURCES_DIR"], !explicit.isEmpty { return nil }
        if let bundled = bundleResources?.appendingPathComponent("ghostty").path, exists(bundled + "/themes") { return bundled }
        if exists(ghosttyApp + "/themes") { return ghosttyApp }
        return nil
    }

    /// Hand `resourcesDir()` to libghostty (an explicit env value is kept).
    /// Returns what was exported, nil when nothing was.
    @discardableResult
    static func exportResourcesDir() -> String? {
        guard let dir = resourcesDir() else { return nil }
        setenv("GHOSTTY_RESOURCES_DIR", dir, 0)
        return dir
    }

    // MARK: the user's Ghostty config

    /// The config files Ghostty itself loads on macOS, in its order.
    static func userConfigURLs(home: URL, env: [String: String] = ProcessInfo.processInfo.environment) -> [URL] {
        let xdg = env["XDG_CONFIG_HOME"].flatMap { $0.isEmpty ? nil : URL(fileURLWithPath: $0) } ?? home.appendingPathComponent(".config")
        return [xdg.appendingPathComponent("ghostty/config"),
                home.appendingPathComponent("Library/Application Support/com.mitchellh.ghostty/config")]
    }

    /// The font keys the user's own config sets (`font-family`, `font-size`,
    /// …). `config-file` includes are not followed — documented limitation.
    static func readUserConfigKeys(home: URL = FileManager.default.homeDirectoryForCurrentUser) -> Set<String> {
        var keys = Set<String>()
        for url in userConfigURLs(home: home) {
            if let text = try? String(contentsOf: url, encoding: .utf8) { keys.formUnion(fontKeys(in: text)) }
        }
        return keys
    }

    /// Pure: the `font-*` keys assigned in one Ghostty config text. Comments
    /// and blank values are ignored (`font-family =` alone is Ghostty's "reset
    /// the list", not a choice).
    static func fontKeys(in text: String) -> Set<String> {
        var keys = Set<String>()
        for raw in text.split(whereSeparator: \.isNewline) {
            let line = raw.trimmingCharacters(in: .whitespaces)
            guard line.hasPrefix("font-"), let eq = line.firstIndex(of: "=") else { continue }
            let key = line[..<eq].trimmingCharacters(in: .whitespaces)
            let value = line[line.index(after: eq)...].trimmingCharacters(in: .whitespaces)
            if !value.isEmpty { keys.insert(key) }
        }
        return keys
    }

    // MARK: the overrides

    /// The ghostty config lines SmoothFlow appends after the user's files.
    /// `userKeys` is what their config already sets (see precedence above).
    static func overrides(settings: TerminalSettings, userKeys: Set<String>) -> String {
        var lines: [String] = []
        let family: String? = settings.family ?? (userKeys.contains("font-family") ? nil : bundledFamily)
        if let family {
            // `font-family` is a LIST in Ghostty (repeat = fallback), so clear
            // each key before setting it, or we would only append a fallback.
            for key in ["font-family", "font-family-bold", "font-family-italic", "font-family-bold-italic"] {
                lines.append("\(key) = ")
                lines.append("\(key) = \(family)")
            }
        }
        if let size = settings.size {
            lines.append("font-size = \(format(size))")
        } else if family == bundledFamily, !userKeys.contains("font-size") {
            lines.append("font-size = \(format(defaultSize))")
        }
        if !settings.ligatures {
            for feature in ["calt", "liga", "dlig"] { lines.append("font-feature = -\(feature)") }
        }
        return lines.isEmpty ? "" : lines.joined(separator: "\n") + "\n"
    }

    private static func format(_ size: Double) -> String {
        size == size.rounded() ? String(Int(size)) : String(size)
    }

    /// Installed fixed-pitch families for the picker, bundled one first.
    static func availableFamilies(manager: NSFontManager = .shared) -> [String] {
        let names = manager.availableFontNames(with: .fixedPitchFontMask) ?? []
        var families = Set<String>()
        for n in names {
            if let f = NSFont(name: n, size: 12)?.familyName { families.insert(f) }
        }
        families.remove(bundledFamily)
        return [bundledFamily] + families.sorted()
    }
}

/// Settings ▸ Terminal (th-bcd819). `nil` family / size = not chosen here,
/// which lets the user's Ghostty config, then the bundled default, decide.
struct TerminalSettings: Equatable {
    var family: String?
    var size: Double?
    var ligatures = true

    static let familyKey = "terminal.fontFamily"
    static let sizeKey = "terminal.fontSize"
    static let ligaturesKey = "terminal.ligatures"

    static func load(_ d: UserDefaults = .standard) -> TerminalSettings {
        var s = TerminalSettings()
        if let f = d.string(forKey: familyKey), !f.isEmpty { s.family = f }
        if d.object(forKey: sizeKey) != nil, TerminalFont.sizeRange.contains(d.double(forKey: sizeKey)) { s.size = d.double(forKey: sizeKey) }
        if d.object(forKey: ligaturesKey) != nil { s.ligatures = d.bool(forKey: ligaturesKey) }
        return s
    }

    func save(_ d: UserDefaults = .standard) {
        if let family { d.set(family, forKey: Self.familyKey) } else { d.removeObject(forKey: Self.familyKey) }
        if let size { d.set(size, forKey: Self.sizeKey) } else { d.removeObject(forKey: Self.sizeKey) }
        d.set(ligatures, forKey: Self.ligaturesKey)
    }
}

import AppKit
import XCTest
@testable import SmoothFlow

/// th-bcd819: the bundled Nerd Font registers for this process, and the
/// ghostty overrides honor Settings ▸ Terminal → the user's Ghostty config →
/// the bundled default, in that order.
final class TerminalFontTests: XCTestCase {
    func testBundledFacesRegisterAndResolve() {
        let usable = TerminalFont.registerBundled()
        XCTAssertEqual(usable, 4, "Regular, Bold, Italic, BoldItalic ship in Resources/Fonts")
        XCTAssertEqual(TerminalFont.registeredFamily, TerminalFont.bundledFamily)
        let regular = NSFont(name: TerminalFont.bundledPostScriptRegular, size: 13)
        XCTAssertEqual(regular?.familyName, TerminalFont.bundledFamily)
        XCTAssertEqual(NSFont(name: "JetBrainsMonoNF-Bold", size: 13)?.familyName, TerminalFont.bundledFamily)
        // (CoreText does not flag the Nerd Font build fixed-pitch — its icon
        // glyphs are wider — so `isFixedPitch` is deliberately not asserted.)
        // A Nerd glyph (nf-pl-branch) is in the face — the whole point.
        let branch = "\u{e0a0}".unicodeScalars.first!
        XCTAssertTrue(regular?.coveredCharacterSet.contains(branch) ?? false, "Nerd Font private-use glyphs are covered")
        XCTAssertEqual(TerminalFont.registerBundled(), 4, "re-registering is idempotent (already registered counts)")
        XCTAssertEqual(TerminalFont.availableFamilies().first, TerminalFont.bundledFamily, "the picker lists the bundled family first")
    }

    func testDefaultOverridesPinTheBundledFamily() {
        let o = TerminalFont.overrides(settings: TerminalSettings(), userKeys: [])
        for key in ["font-family", "font-family-bold", "font-family-italic", "font-family-bold-italic"] {
            XCTAssertTrue(o.contains("\(key) = \n"), "\(key) is cleared first — Ghostty treats a repeat as a fallback, not a replacement")
            XCTAssertTrue(o.contains("\(key) = JetBrainsMono Nerd Font\n"), key)
        }
        XCTAssertTrue(o.contains("font-size = 13\n"))
        XCTAssertFalse(o.contains("font-feature"), "ligatures stay on by default")
    }

    func testUsersGhosttyFontWinsOverTheBundledDefault() {
        let o = TerminalFont.overrides(settings: TerminalSettings(), userKeys: ["font-family"])
        XCTAssertFalse(o.contains("font-family"), "their terminal, their font: \(o)")
        XCTAssertFalse(o.contains("font-size"), "their font, their size too — Ghostty's own 13 pt default applies")
        let both = TerminalFont.overrides(settings: TerminalSettings(), userKeys: ["font-family", "font-size"])
        XCTAssertEqual(both, "", "nothing to add when their config covers both")
        let sizeOnly = TerminalFont.overrides(settings: TerminalSettings(), userKeys: ["font-size"])
        XCTAssertTrue(sizeOnly.contains("font-family = JetBrainsMono Nerd Font"))
        XCTAssertFalse(sizeOnly.contains("font-size"), "their size is not stomped")
    }

    func testSettingsBeatTheUsersGhosttyConfig() {
        let s = TerminalSettings(family: "Menlo", size: 15, ligatures: false)
        let o = TerminalFont.overrides(settings: s, userKeys: ["font-family", "font-size"])
        XCTAssertTrue(o.contains("font-family = Menlo\n"))
        XCTAssertTrue(o.contains("font-family-bold = Menlo\n"))
        XCTAssertTrue(o.contains("font-size = 15\n"))
        XCTAssertTrue(o.contains("font-feature = -calt\n") && o.contains("font-feature = -liga\n") && o.contains("font-feature = -dlig\n"))
        XCTAssertTrue(TerminalFont.overrides(settings: TerminalSettings(size: 12.5), userKeys: []).contains("font-size = 12.5\n"))
    }

    func testFontKeysAreParsedFromGhosttyConfigText() {
        let text = """
        # font-family = Commented Out
        theme = catppuccin-mocha
          font-family=Fira Code
        font-family-bold = Fira Code Bold
        font-size = 14
        font-thicken =
        """
        XCTAssertEqual(TerminalFont.fontKeys(in: text), ["font-family", "font-family-bold", "font-size"], "comments and blank resets do not count")
        XCTAssertEqual(TerminalFont.fontKeys(in: ""), [])
    }

    func testUserConfigLocationsFollowGhostty() {
        let home = URL(fileURLWithPath: "/Users/u")
        let urls = TerminalFont.userConfigURLs(home: home, env: [:]).map(\.path)
        XCTAssertEqual(urls, ["/Users/u/.config/ghostty/config", "/Users/u/Library/Application Support/com.mitchellh.ghostty/config"])
        XCTAssertEqual(TerminalFont.userConfigURLs(home: home, env: ["XDG_CONFIG_HOME": "/x/cfg"]).first?.path, "/x/cfg/ghostty/config")
        // Reading a home with no Ghostty config is not an error.
        let empty = FileManager.default.temporaryDirectory.appendingPathComponent("no-ghostty-\(UUID().uuidString.prefix(6))")
        XCTAssertEqual(TerminalFont.readUserConfigKeys(home: empty), [])
    }

    func testSettingsRoundTripThroughDefaults() throws {
        let suite = "terminal-font-tests-\(UUID().uuidString.prefix(6))"
        let d = try XCTUnwrap(UserDefaults(suiteName: suite))
        defer { d.removePersistentDomain(forName: suite) }
        XCTAssertEqual(TerminalSettings.load(d), TerminalSettings(), "nothing chosen = defaults")
        let chosen = TerminalSettings(family: "Menlo", size: 16, ligatures: false)
        chosen.save(d)
        XCTAssertEqual(TerminalSettings.load(d), chosen)
        TerminalSettings().save(d)
        XCTAssertEqual(TerminalSettings.load(d), TerminalSettings(), "clearing removes the explicit keys")
        d.set(400.0, forKey: TerminalSettings.sizeKey)
        XCTAssertNil(TerminalSettings.load(d).size, "an out-of-range size is ignored")
    }
}

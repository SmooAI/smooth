import XCTest
@testable import SmoothFlow

/// th-bcd819, the suspenders: the child daemon's locale (tmux draws `_` for
/// every non-ASCII cell on a client without a UTF-8 locale) and the resources
/// dir themes resolve under.
final class TerminalLocaleTests: XCTestCase {
    func testUTF8LocaleIsAddedOnlyWhenMissing() {
        let known: (String) -> Bool = { $0 == "en_US.UTF-8" || $0 == "de_DE.UTF-8" }
        XCTAssertEqual(DaemonManager.utf8Locale(env: [:], identifier: "en_US", known: known), ["LANG": "en_US.UTF-8", "LC_CTYPE": "en_US.UTF-8"])
        XCTAssertEqual(DaemonManager.utf8Locale(env: ["PATH": "/bin"], identifier: "de_DE", known: known)["LANG"], "de_DE.UTF-8")
        XCTAssertEqual(DaemonManager.utf8Locale(env: [:], identifier: "en-GB", known: known)["LANG"], "en_US.UTF-8", "an unknown locale falls back to en_US")
        XCTAssertEqual(DaemonManager.utf8Locale(env: [:], identifier: "en_US@rg=gbzzzz", known: known)["LANG"], "en_US.UTF-8", "the @variant is dropped")
        XCTAssertEqual(DaemonManager.utf8Locale(env: [:], identifier: "en", known: known)["LANG"], "en_US.UTF-8", "a bare language is not a locale name")
        for present in [["LANG": "en_US.UTF-8"], ["LC_ALL": "C.UTF-8"], ["LC_CTYPE": "en_GB.utf8"], ["LANG": "C", "LC_CTYPE": "UTF-8"]] {
            XCTAssertEqual(DaemonManager.utf8Locale(env: present, identifier: "en_US", known: known), [:], "already UTF-8: \(present)")
        }
        XCTAssertEqual(DaemonManager.utf8Locale(env: ["LANG": "C"], identifier: "en_US", known: known)["LANG"], "en_US.UTF-8", "LANG=C is not UTF-8")
    }

    func testTheHostLocaleResolvesToARealFile() {
        // Whatever this Mac's locale is, the real `known` check yields a name
        // /usr/share/locale actually has (or the en_US fallback).
        let name = DaemonManager.utf8Locale(env: [:])["LANG"]!
        XCTAssertTrue(FileManager.default.fileExists(atPath: "/usr/share/locale/\(name)"), name)
    }

    func testResourcesDirPrefersExplicitThenBundledThenGhosttyApp() {
        let bundle = URL(fileURLWithPath: "/App/Contents/Resources")
        let exists: (String) -> Bool = { $0 == "/App/Contents/Resources/ghostty/themes" || $0 == "/G/ghostty/themes" }
        XCTAssertNil(TerminalFont.resourcesDir(env: ["GHOSTTY_RESOURCES_DIR": "/mine"], bundleResources: bundle, ghosttyApp: "/G/ghostty", exists: exists), "explicit env is respected")
        XCTAssertEqual(TerminalFont.resourcesDir(env: [:], bundleResources: bundle, ghosttyApp: "/G/ghostty", exists: exists), "/App/Contents/Resources/ghostty")
        XCTAssertEqual(TerminalFont.resourcesDir(env: [:], bundleResources: nil, ghosttyApp: "/G/ghostty", exists: exists), "/G/ghostty")
        XCTAssertNil(TerminalFont.resourcesDir(env: [:], bundleResources: nil, ghosttyApp: "/none", exists: exists))
    }

    func testThemesShipInTheBundle() {
        let dir = TerminalFont.resourcesDir(env: [:], ghosttyApp: "/none")
        XCTAssertEqual(dir, Bundle.main.resourceURL!.appendingPathComponent("ghostty").path, "the bundled copy wins")
        XCTAssertTrue(FileManager.default.fileExists(atPath: dir! + "/themes/Catppuccin Mocha"), "a well-known theme is there")
        // What GhosttyRuntime does before ghostty_init (the test host never
        // builds a surface, so call it here and read the raw environ).
        XCTAssertEqual(TerminalFont.exportResourcesDir(), dir)
        XCTAssertEqual(getenv("GHOSTTY_RESOURCES_DIR").map { String(cString: $0) }, dir, "exported for libghostty")
    }
}

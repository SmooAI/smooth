import AppKit
@testable import SmoothFlow
import XCTest

/// SMOODEV-4266: what ⌘-click may do with text a program printed.
final class TerminalLinkTests: XCTestCase {
    private func files(_ existing: [String], dirs: [String] = [], exec: [String] = []) -> TerminalLink.Files {
        TerminalLink.Files(exists: { existing.contains($0) || dirs.contains($0) }, isDirectory: { dirs.contains($0) }, isExecutable: { exec.contains($0) })
    }

    func testWebAndMailLinksOpen() throws {
        for raw in ["https://smoo.ai/x?y=1", "http://localhost:3000/", "mailto:a@b.co", "ssh://host"] {
            XCTAssertEqual(TerminalLink.resolve(raw, cwd: nil, files: files([])), .open(try XCTUnwrap(URL(string: raw))), raw)
        }
    }

    func testOtherSchemesAreRefused() {
        for raw in ["javascript://alert(1)", "x-apple.systempreferences://foo", "vscode://file/etc/passwd", "smb://evil/share"] {
            XCTAssertEqual(TerminalLink.resolve(raw, cwd: nil, files: files([])), .refused(raw), raw)
        }
    }

    func testAPathWithAColonIsNotAScheme() {
        let f = files(["/repo/foo.rs"])
        XCTAssertEqual(TerminalLink.resolve("foo.rs:12", cwd: "/repo", files: f), .open(URL(fileURLWithPath: "/repo/foo.rs")))
    }

    func testPathsResolveAgainstTheDirectory() {
        let f = files(["/repo/src/a.rs", "/etc/hosts"], dirs: ["/repo/src"])
        XCTAssertEqual(TerminalLink.resolve("./src/a.rs", cwd: "/repo", files: f), .open(URL(fileURLWithPath: "/repo/src/a.rs")))
        XCTAssertEqual(TerminalLink.resolve("/etc/hosts", cwd: nil, files: f), .open(URL(fileURLWithPath: "/etc/hosts")))
        XCTAssertEqual(TerminalLink.resolve("src", cwd: "/repo", files: f), .open(URL(fileURLWithPath: "/repo/src")))
        XCTAssertEqual(TerminalLink.resolve("src/a.rs", cwd: nil, files: f), .refused("src/a.rs"), "relative, with nothing to resolve against")
    }

    func testHomeExpandsAndFileURLsDecode() {
        let home = NSHomeDirectory()
        let f = files(["\(home)/notes.md", "/tmp/a b.txt"])
        XCTAssertEqual(TerminalLink.resolve("~/notes.md", cwd: nil, files: f), .open(URL(fileURLWithPath: "\(home)/notes.md")))
        XCTAssertEqual(TerminalLink.resolve("file:///tmp/a%20b.txt", cwd: nil, files: f), .open(URL(fileURLWithPath: "/tmp/a b.txt")))
    }

    func testCompilerLineSuffixes() {
        let f = files(["/repo/main.rs"])
        XCTAssertEqual(TerminalLink.resolve("/repo/main.rs:12:3", cwd: nil, files: f), .open(URL(fileURLWithPath: "/repo/main.rs")))
        XCTAssertEqual(TerminalLink.strippingLineSuffix("/a/b:7"), "/a/b")
        XCTAssertEqual(TerminalLink.strippingLineSuffix("/a/b:c"), "/a/b:c")
    }

    func testAMissingFileIsRefused() {
        XCTAssertEqual(TerminalLink.resolve("/no/such/file", cwd: nil, files: files([])), .refused("/no/such/file"))
    }

    func testNothingALinkPointsAtIsLaunched() {
        let f = files(["/tmp/run.sh", "/tmp/evil.command"], dirs: ["/Applications/Evil.app"], exec: ["/tmp/run.sh", "/tmp/evil.command"])
        XCTAssertEqual(TerminalLink.resolve("/tmp/run.sh", cwd: nil, files: f), .reveal(URL(fileURLWithPath: "/tmp/run.sh")))
        XCTAssertEqual(TerminalLink.resolve("/tmp/evil.command", cwd: nil, files: f), .reveal(URL(fileURLWithPath: "/tmp/evil.command")))
        XCTAssertEqual(TerminalLink.resolve("/Applications/Evil.app", cwd: nil, files: f), .reveal(URL(fileURLWithPath: "/Applications/Evil.app")))
        XCTAssertEqual(TerminalLink.resolve("file:///Applications/Evil.app", cwd: nil, files: f), .reveal(URL(fileURLWithPath: "/Applications/Evil.app")))
    }

    func testBlankIsRefused() {
        XCTAssertEqual(TerminalLink.resolve("  ", cwd: nil, files: files([])), .refused("  "))
    }
}

/// ⌘-click end to end: libghostty finds the link in the output, raises
/// MOUSE_OVER_LINK and OPEN_URL, and the surface resolves what it was given.
@MainActor
final class TerminalLinkSurfaceTests: XCTestCase {
    private func wait(_ what: String, until done: () -> Bool) {
        let end = Date().addingTimeInterval(10)
        while !done(), Date() < end {
            RunLoop.current.run(until: Date().addingTimeInterval(0.02))
        }
        XCTAssertTrue(done(), "timed out waiting for \(what)")
    }

    private func mouse(_ type: NSEvent.EventType, at p: NSPoint, in window: NSWindow) throws -> NSEvent {
        try XCTUnwrap(NSEvent.mouseEvent(with: type, location: p, modifierFlags: .command, timestamp: 0, windowNumber: window.windowNumber,
                                         context: nil, eventNumber: 0, clickCount: 1, pressure: 1))
    }

    /// ⌘-click at the first row of the screen, and return what the surface tried to do.
    private func clickFirstRow(printing line: String, cwd: String? = nil) throws -> (hovered: String?, target: TerminalLink.Target?) {
        let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 800, height: 400), styleMask: [.titled], backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false
        defer { window.close() }
        let surface = TerminalSurfaceView(sessionId: "link-test")
        surface.frame = NSRect(x: 0, y: 0, width: 800, height: 400)
        window.contentView?.addSubview(surface)
        surface.fallbackDirectory = { cwd }
        var target: TerminalLink.Target?
        surface.performLink = { target = $0 }
        surface.feed(Data((line + "\r\n").utf8))
        RunLoop.current.run(until: Date().addingTimeInterval(0.3))

        // A few points in: past the padding, inside the link's first characters.
        let p = surface.convert(NSPoint(x: 24, y: surface.bounds.height - 12), to: nil)
        surface.mouseMoved(with: try mouse(.mouseMoved, at: p, in: window))
        wait("the hover to report the link") { surface.hoveredLink != nil }
        let hovered = surface.hoveredLink
        surface.mouseDown(with: try mouse(.leftMouseDown, at: p, in: window))
        surface.mouseUp(with: try mouse(.leftMouseUp, at: p, in: window))
        wait("the click to reach OPEN_URL") { target != nil }
        return (hovered, target)
    }

    func testCommandClickOpensAURL() throws {
        try XCTSkipIf(GhosttyRuntime.shared.app == nil, "no ghostty app in this host")
        let url = "https://smoo.ai/docs?x=1"
        let got = try clickFirstRow(printing: url)
        XCTAssertEqual(got.hovered, url)
        XCTAssertEqual(got.target, .open(try XCTUnwrap(URL(string: url))))
    }

    func testCommandClickOpensAFilePath() throws {
        try XCTSkipIf(GhosttyRuntime.shared.app == nil, "no ghostty app in this host")
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent("link-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: dir) }
        let file = dir.appendingPathComponent("notes.txt")
        try Data("x".utf8).write(to: file)
        let got = try clickFirstRow(printing: file.path)
        guard case let .open(url) = got.target else { return XCTFail("\(String(describing: got.target))") }
        XCTAssertEqual(url.resolvingSymlinksInPath().path, file.resolvingSymlinksInPath().path)
    }
}

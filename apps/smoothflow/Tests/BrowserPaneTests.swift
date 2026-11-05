import AppKit
import Network
@testable import SmoothFlow
import WebKit
import XCTest

/// SMOODEV-4270: what the URL bar accepts, what a page may do, and how a browser
/// pane sits in the tab model and the saved layout.
final class BrowserURLTests: XCTestCase {
    private func url(_ typed: String) -> String? {
        if case let .ok(u) = BrowserURL.parse(typed) { return u.absoluteString }
        return nil
    }

    func testWebURLsOpen() {
        XCTAssertEqual(url("https://example.com/a?b=c#d"), "https://example.com/a?b=c#d")
        XCTAssertEqual(url("http://example.com"), "http://example.com")
        XCTAssertEqual(url("  HTTPS://Example.com  "), "HTTPS://Example.com")
    }

    func testABareHostGetsHttps() {
        XCTAssertEqual(url("example.com"), "https://example.com")
        XCTAssertEqual(url("example.com/a:b"), "https://example.com/a:b")
        XCTAssertEqual(url("example.com:8443/x"), "https://example.com:8443/x")
    }

    func testALocalHostGetsHttp() {
        for (typed, want) in [("localhost:3000", "http://localhost:3000"), ("localhost", "http://localhost"), ("smoo-hub:8787/x", "http://smoo-hub:8787/x"),
                              ("127.0.0.1:8080", "http://127.0.0.1:8080"), ("192.168.1.5", "http://192.168.1.5"), ("172.20.0.1", "http://172.20.0.1"),
                              ("10.0.0.2:9", "http://10.0.0.2:9"), ("mac.local", "http://mac.local"), ("app.localhost:3000", "http://app.localhost:3000"),
                              ("[::1]:3000", "http://[::1]:3000")] {
            XCTAssertEqual(url(typed), want, typed)
        }
        XCTAssertEqual(url("172.32.0.1"), "https://172.32.0.1", "outside the private range")
    }

    func testOtherSchemesAreRefused() {
        for typed in ["javascript:alert(1)", "JavaScript:alert(1)", " javascript:alert(1)", "javascript://%0aalert(1)", "javascript:1",
                      "file:///etc/passwd", "FILE:///etc/passwd", "file:/etc/passwd", "file:80", "file://localhost/etc/passwd",
                      "data:text/html,<script>alert(1)</script>", "about:blank", "blob:https://example.com/x", "view-source:https://example.com",
                      "vbscript:x", "mailto:a@example.com", "ssh://example.com", "ftp://example.com", "tel:5551212", "zoommtg://x", "foo:bar",
                      "http:example.com", "http:80", "https:/example.com"] {
            XCTAssertNil(url(typed), typed)
            if case let .rejected(why) = BrowserURL.parse(typed) { XCTAssertFalse(why.isEmpty) }
        }
    }

    func testDisguisedAndPathShapedInputIsRefused() {
        for typed in ["", "   ", "java\tscript:alert(1)", "java\nscript:alert(1)", "\u{0}javascript:alert(1)", "two words", "https://exa mple.com",
                      "https:\\\\example.com", "/etc/passwd", "~/x", "./x", "../x", "//example.com", "https://", "http:///x",
                      "https://github.com@evil.example", "https://user:pass@example.com", "me@example.com"] {
            XCTAssertNil(url(typed), typed.debugDescription)
        }
    }

    func testTopFrameNavigationIsWebOnly() throws {
        for good in ["https://example.com", "http://localhost:3000/"] {
            XCTAssertTrue(BrowserURL.allowsNavigation(try XCTUnwrap(URL(string: good)), mainFrame: true), good)
        }
        for bad in ["file:///etc/passwd", "javascript:alert(1)", "data:text/html,x", "about:blank", "blob:https://example.com/x", "zoommtg://x", "https://a@example.com"] {
            XCTAssertFalse(BrowserURL.allowsNavigation(try XCTUnwrap(URL(string: bad)), mainFrame: true), bad)
        }
    }

    func testAPageMayBuildFramesButNotReachOutOfThem() throws {
        for good in ["about:srcdoc", "about:blank", "data:text/html,x", "blob:https://example.com/x", "https://example.com"] {
            XCTAssertTrue(BrowserURL.allowsNavigation(try XCTUnwrap(URL(string: good)), mainFrame: false), good)
        }
        for bad in ["file:///etc/passwd", "javascript:alert(1)", "zoommtg://x"] {
            XCTAssertFalse(BrowserURL.allowsNavigation(try XCTUnwrap(URL(string: bad)), mainFrame: false), bad)
        }
    }

    /// The terminal side of the hook: web pages go to a pane, the rest stays with
    /// `TerminalLink.perform`, and the setting sends web pages back there too.
    func testOnlyWebLinksGoToAPane() throws {
        let page = try XCTUnwrap(URL(string: "https://example.com/x"))
        XCTAssertEqual(BrowserURL.paneLink(for: .open(page), enabled: true), page)
        XCTAssertNil(BrowserURL.paneLink(for: .open(page), enabled: false))
        XCTAssertNil(BrowserURL.paneLink(for: .open(try XCTUnwrap(URL(string: "mailto:a@example.com"))), enabled: true))
        XCTAssertNil(BrowserURL.paneLink(for: .open(URL(fileURLWithPath: "/tmp/x.txt")), enabled: true))
        XCTAssertNil(BrowserURL.paneLink(for: .reveal(URL(fileURLWithPath: "/tmp/x.app")), enabled: true))
        XCTAssertNil(BrowserURL.paneLink(for: .refused("x"), enabled: true))
    }

    func testTheLinkSettingDefaultsOn() throws {
        let name = "smoothflow.tests.browser.\(UUID().uuidString)"
        let d = try XCTUnwrap(UserDefaults(suiteName: name))
        defer { d.removePersistentDomain(forName: name) }
        XCTAssertTrue(BrowserSettings.opensTerminalLinks(d))
        BrowserSettings.setOpensTerminalLinks(false, d)
        XCTAssertFalse(BrowserSettings.opensTerminalLinks(d))
    }

    func testBrowserChordsAreFreeAndOnlyTheOpenerWorksEverywhere() {
        XCTAssertTrue(Keymap.default.conflicts.isEmpty)
        XCTAssertFalse(FlowAction.openBrowser.needsFocusedBrowser)
        for a in [FlowAction.browserLocation, .browserBack, .browserForward, .browserReload] {
            XCTAssertTrue(a.needsFocusedBrowser, a.rawValue)
            XCTAssertEqual(a.category, .browser)
        }
    }
}

final class BrowserPaneModelTests: XCTestCase {
    func testAnEmptyPaneBecomesTheBrowser() {
        var t = SurfaceTab(id: 1, pane: PaneID.next())
        let pane = t.focused
        XCTAssertEqual(t.openBrowser(url: ""), pane)
        XCTAssertEqual(t.panes, [pane])
        XCTAssertEqual(t.browsers[pane], "")
    }

    func testABusyOrFullPaneIsSplitRight() {
        var t = SurfaceTab(id: 1, pane: PaneID.next())
        let first = t.focused
        let beside = t.openBrowser(url: "https://example.com", busy: true)
        XCTAssertEqual(t.root, .split(horizontal: true, first: .leaf(first), second: .leaf(beside), fraction: 0.5))

        var u = SurfaceTab(id: 2, pane: PaneID.next())
        let shell = u.focused
        u.sessions[shell] = "fs-a"
        let pane = u.openBrowser(url: "https://example.com")
        XCTAssertNotEqual(pane, shell)
        XCTAssertEqual(u.focused, pane)
        XCTAssertNil(u.sessions[pane], "not a second view of the session it split from")
        XCTAssertEqual(u.sessions[shell], "fs-a")
    }

    func testOpeningClearsZoom() {
        var t = SurfaceTab(id: 1, pane: PaneID.next())
        t.sessions[t.focused] = "fs-a"
        _ = t.split(.right)
        t.toggleZoom()
        t.openBrowser(url: "https://example.com")
        XCTAssertNil(t.zoomed)
    }

    func testClosingAPaneForgetsItsBrowser() {
        var t = SurfaceTab(id: 1, pane: PaneID.next())
        t.sessions[t.focused] = "fs-a"
        let pane = t.openBrowser(url: "https://example.com")
        XCTAssertTrue(t.closeFocused())
        XCTAssertNil(t.browsers[pane])
    }

    func testTheTabIsNamedForThePage() {
        var t = SurfaceTab(id: 1, pane: PaneID.next())
        t.openBrowser(url: "")
        XCTAssertEqual(t.title { _ in "no" }, "browser")
        t.browsers[t.focused] = "https://example.com/a"
        XCTAssertEqual(t.title { _ in "no" }, "example.com")
    }

    /// The sidebar used to write into the focused pane, which would drop a page.
    func testASessionNeverOverwritesAFocusedBrowser() {
        var t = SurfaceTab(id: 1, pane: PaneID.next())
        let shell = t.focused
        t.sessions[shell] = "fs-a"
        let browser = t.openBrowser(url: "https://example.com")
        XCTAssertTrue(t.place(session: "fs-b"))
        XCTAssertEqual(t.browsers[browser], "https://example.com")
        XCTAssertEqual(t.sessions[shell], "fs-b")
        XCTAssertEqual(t.focused, shell)
    }

    func testASessionBesideABrowserOnlyTab() {
        var t = SurfaceTab(id: 1, pane: PaneID.next())
        let browser = t.openBrowser(url: "https://example.com")
        XCTAssertTrue(t.place(session: "fs-a"))
        XCTAssertEqual(t.panes.count, 2)
        XCTAssertNotEqual(t.focused, browser)
        XCTAssertEqual(t.sessions[t.focused], "fs-a")
        XCTAssertNil(t.sessions[browser])
    }

    func testPlacingASessionKeepsItsOldRules() {
        var t = SurfaceTab(id: 1, pane: PaneID.next())
        let a = t.focused
        XCTAssertTrue(t.place(session: "fs-a"))
        XCTAssertFalse(t.place(session: "fs-a"), "already focused")
        let b = t.split(.right)
        t.sessions[b] = nil
        XCTAssertTrue(t.place(session: "fs-a"), "moves to the pane already showing it")
        XCTAssertEqual(t.focused, a)
        XCTAssertNil(t.sessions[b], "and does not stack a second view")
    }

    // MARK: saved layout

    private func roundTrip(_ layout: SavedLayout) throws -> SavedLayout {
        try JSONDecoder().decode(SavedLayout.self, from: JSONEncoder().encode(layout))
    }

    func testAPaneKeepsItsURLAcrossARelaunch() throws {
        var t = SurfaceTab(id: 1, pane: PaneID.next())
        t.sessions[t.focused] = "fs-a"
        t.openBrowser(url: "https://example.com/a?b=c")
        let (tabs, _) = try XCTUnwrap(try roundTrip(SavedLayout([t], active: 0)).restore(firstTabId: 1))
        XCTAssertEqual(tabs[0].panes.map { tabs[0].browsers[$0] }, [nil, "https://example.com/a?b=c"])
        XCTAssertEqual(tabs[0].panes.map { tabs[0].sessions[$0] }, ["fs-a", nil])
        XCTAssertEqual(tabs[0].focused, tabs[0].panes[1])
    }

    func testABlankBrowserComesBackBlank() throws {
        var t = SurfaceTab(id: 1, pane: PaneID.next())
        t.openBrowser(url: "")
        let (tabs, _) = try XCTUnwrap(try roundTrip(SavedLayout([t], active: 0)).restore(firstTabId: 1))
        XCTAssertEqual(tabs[0].browsers[tabs[0].focused], "")
    }

    /// A layout written before browser panes, byte for byte what that build saved.
    func testALayoutFromBeforeBrowserPanesStillDecodes() throws {
        let old = #"{"version":1,"active":0,"tabs":[{"root":{"split":{"horizontal":true,"first":{"leaf":{}},"second":{"leaf":{}},"fraction":0.25}},"focused":1,"sessions":{"0":"fs-a"}}]}"#
        let layout = try JSONDecoder().decode(SavedLayout.self, from: Data(old.utf8))
        let (tabs, _) = try XCTUnwrap(layout.restore(firstTabId: 1))
        XCTAssertEqual(tabs[0].panes.count, 2)
        XCTAssertEqual(tabs[0].sessions[tabs[0].panes[0]], "fs-a")
        XCTAssertTrue(tabs[0].browsers.isEmpty)
    }

    func testNoBrowsersLeavesTheJSONAsItWas() throws {
        var t = SurfaceTab(id: 1, pane: PaneID.next())
        t.sessions[t.focused] = "fs-a"
        let data = try JSONEncoder().encode(SavedLayout([t], active: 0))
        let json = try XCTUnwrap(JSONSerialization.jsonObject(with: data) as? [String: Any])
        let first = try XCTUnwrap((json["tabs"] as? [[String: Any]])?.first)
        XCTAssertNil(first["browsers"])
        XCTAssertEqual(json["version"] as? Int, 1)
    }

    /// The saved value is editable, and a URL in it gets loaded.
    func testASavedURLPassesTheSameGate() throws {
        let saved = SavedTab(root: .split(horizontal: true, first: .leaf, second: .leaf, fraction: 0.5), focused: 0, browsers: [0: "file:///etc/passwd", 1: "javascript:alert(1)"])
        let tab = saved.restore(id: 1)
        XCTAssertTrue(tab.browsers.isEmpty)
        XCTAssertEqual(tab.panes.count, 2, "the panes stay, empty")
    }

    func testABrowserEntryWinsOverASessionOnTheSamePane() {
        let saved = SavedTab(root: .leaf, focused: 0, sessions: [0: "fs-a"], browsers: [0: "https://example.com"])
        let tab = saved.restore(id: 1)
        XCTAssertNil(tab.sessions[tab.focused])
        XCTAssertEqual(tab.browsers[tab.focused], "https://example.com")
    }
}

/// A real web view against a loopback server: the delegate rules have to hold
/// where they count, inside WebKit, and a wrong delegate signature would only
/// show up here.
@MainActor
final class BrowserPaneViewTests: XCTestCase {
    private var listener: NWListener!
    private var port: UInt16 = 0
    private var window: NSWindow!
    private var pages: [String: String] = [:]

    override func setUp() async throws {
        try await super.setUp()
        pages = ["/": "<h1>home</h1>", "/two": "<h1>two</h1>"]
        let served = pages
        listener = try NWListener(using: .tcp, on: .any)
        listener.newConnectionHandler = { conn in Self.serve(conn, pages: served) }
        listener.start(queue: .global())
        let deadline = Date().addingTimeInterval(10)
        while listener.port == nil || listener.port?.rawValue == 0, Date() < deadline {
            try await Task.sleep(nanoseconds: 20_000_000)
        }
        port = try XCTUnwrap(listener.port?.rawValue)
        window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 800, height: 600), styleMask: [.titled], backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false
    }

    override func tearDown() async throws {
        listener.cancel()
        window.close()
        try await super.tearDown()
    }

    private nonisolated static func serve(_ conn: NWConnection, pages: [String: String]) {
        conn.start(queue: .global())
        conn.receive(minimumIncompleteLength: 1, maximumLength: 8192) { data, _, _, _ in
            let request = data.flatMap { String(data: $0, encoding: .utf8) } ?? ""
            let path = request.split(separator: " ").dropFirst().first.map(String.init) ?? "/"
            let response: String
            if path == "/redirect-to-file" {
                response = "HTTP/1.1 302 Found\r\nLocation: file:///etc/passwd\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            } else if path == "/download" {
                response = "HTTP/1.1 200 OK\r\nContent-Type: application/x-smoothflow-test\r\nContent-Length: 2\r\nConnection: close\r\n\r\nhi"
            } else {
                let body = pages[path] ?? "<h1>nope</h1>"
                response = "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: \(body.utf8.count)\r\nConnection: close\r\n\r\n\(body)"
            }
            conn.send(content: Data(response.utf8), completion: .contentProcessed { _ in conn.cancel() })
        }
    }

    private func pane(_ url: String = "") -> BrowserPaneView {
        let p = BrowserPaneView(url: url)
        p.frame = window.contentView!.bounds
        window.contentView!.addSubview(p)
        return p
    }

    private func base(_ path: String = "/") -> String { "http://127.0.0.1:\(port)\(path)" }

    /// Poll the run loop: WebKit calls back on it.
    private func wait(timeout: TimeInterval = 20, _ what: String, _ cond: @MainActor () async -> Bool) async {
        let deadline = Date().addingTimeInterval(timeout)
        while Date() < deadline {
            if await cond() { return }
            try? await Task.sleep(nanoseconds: 50_000_000)
        }
        XCTFail("timed out waiting for \(what)")
    }

    private func script(_ p: BrowserPaneView, _ js: String) async -> Any? {
        try? await p.webView.evaluateJavaScript(js)
    }

    private func loaded(_ p: BrowserPaneView, _ text: String) async {
        await wait("page with \(text)") {
            await self.script(p, "document.body && document.body.innerText") as? String == text
        }
    }

    func testThereIsNoBridgeAndNothingIsKeptOnDisk() async {
        let p = pane(base())
        let q = pane()
        XCTAssertFalse(p.webView.configuration.websiteDataStore.isPersistent)
        XCTAssertTrue(p.webView.configuration.websiteDataStore === q.webView.configuration.websiteDataStore, "panes share one store")
        XCTAssertTrue(p.webView.configuration.userContentController.userScripts.isEmpty)
        XCTAssertFalse(p.webView.configuration.preferences.javaScriptCanOpenWindowsAutomatically)
        await loaded(p, "home")
        let handlers = await script(p, "Object.keys((window.webkit && window.webkit.messageHandlers) || {}).length") as? Int
        XCTAssertEqual(handlers, 0, "a page sees no message handlers")
    }

    /// Plain http has to load, or the App Transport Security key in Info.plist is
    /// wrong, and the bar, the history and `onURLChange` all follow the page.
    func testHttpLoadsAndHistoryWorks() async throws {
        let p = pane()
        window.makeKeyAndOrderFront(nil)
        var seen: [String] = []
        p.onURLChange = { seen.append($0) }
        XCTAssertTrue(p.webView.isHidden, "a blank pane shows its hint")
        XCTAssertTrue(p.submit(base()))
        XCTAssertFalse(p.webView.isHidden)
        await loaded(p, "home")
        XCTAssertEqual(p.locationText, base())
        p.navigate(to: try XCTUnwrap(URL(string: base("/two"))))
        await loaded(p, "two")
        XCTAssertEqual(p.locationText, base("/two"))
        await wait("canGoBack") { p.webView.canGoBack }
        p.goBack()
        await loaded(p, "home")
        await wait("the bar follows back") { p.locationText == self.base() }
        await wait("canGoForward") { p.webView.canGoForward }
        p.goForward()
        await loaded(p, "two")
        XCTAssertEqual(seen.first, base())
        XCTAssertEqual(seen.last, base("/two"))
        XCTAssertNil(p.messageText)
    }

    func testRefusedTextStaysOffTheWire() async {
        let p = pane(base())
        await loaded(p, "home")
        for typed in ["javascript:document.body.innerText='pwned'", "file:///etc/passwd", "data:text/html,pwned"] {
            XCTAssertFalse(p.submit(typed), typed)
            XCTAssertEqual(p.messageText, BrowserURL.onlyWeb, typed)
        }
        await loaded(p, "home")
        XCTAssertEqual(p.currentURL, base(), "nothing moved")
    }

    func testAPageCannotGoToANonWebSchemeByAnyRoute() async {
        let p = pane(base())
        await loaded(p, "home")
        // Past WebKit's own file: block, so this reaches the pane's policy.
        _ = await script(p, "location.href = 'smoothflow-test://x'; 1")
        await wait("the script's refusal") { p.messageText == BrowserURL.onlyWeb }
        XCTAssertEqual(p.webView.url?.absoluteString, base())

        // WebKit stops this one inside the page, before any policy call: the
        // page simply stays where it is.
        _ = await script(p, "location.href = 'file:///etc/passwd'; 1")
        try? await Task.sleep(nanoseconds: 300_000_000)
        XCTAssertEqual(p.webView.url?.absoluteString, base())
        await loaded(p, "home")

        p.navigate(to: URL(string: base("/redirect-to-file"))!)
        await wait("the redirect's refusal") { p.messageText != nil && p.webView.isLoading == false }
        XCTAssertNotEqual(p.webView.url?.scheme, "file")
    }

    func testAResponseItCannotShowIsNotDownloaded() async throws {
        let p = pane(base())
        await loaded(p, "home")
        p.navigate(to: try XCTUnwrap(URL(string: base("/download"))))
        await wait("the refusal") { p.messageText == "The browser pane does not download files." }
    }

    func testAFailedLoadSaysSo() async throws {
        let p = pane()
        // Port 1 on loopback: refused at once.
        p.navigate(to: try XCTUnwrap(URL(string: "http://127.0.0.1:1/")))
        await wait(timeout: 5, "the failure on a blocked port") { p.messageText != nil }
    }

    func testAClosedPortSaysSo() async throws {
        let closed = try NWListener(using: .tcp, on: .any)
        closed.newConnectionHandler = { $0.cancel() }
        closed.start(queue: .global())
        await wait(timeout: 5, "a port") { (closed.port?.rawValue ?? 0) != 0 }
        let number = try XCTUnwrap(closed.port?.rawValue)
        closed.cancel()
        try await Task.sleep(nanoseconds: 100_000_000)
        let p = pane()
        p.navigate(to: try XCTUnwrap(URL(string: "http://127.0.0.1:\(number)/")))
        await wait(timeout: 10, "the failure") { p.messageText != nil }
    }

    /// Typing in the bar while a page moves must not lose the text.
    func testAPageDoesNotOverwriteTheBarWhileItIsBeingEdited() async throws {
        let p = pane(base())
        await loaded(p, "home")
        p.controlTextDidChange(Notification(name: NSControl.textDidChangeNotification))
        p.navigate(to: try XCTUnwrap(URL(string: base("/two"))))
        await loaded(p, "two")
        XCTAssertEqual(p.locationText, base(), "the bar keeps what was there")
        XCTAssertEqual(p.currentURL, base("/two"))
        p.controlTextDidEndEditing(Notification(name: NSControl.textDidEndEditingNotification))
        p.goBack()
        await wait("the bar to follow again") { p.locationText == self.base() }
    }

    func testOnlyAPersonCanGiveThePageTheKeyboard() throws {
        window.makeKeyAndOrderFront(nil)
        let view = NSView(frame: NSRect(x: 100, y: 100, width: 100, height: 100))
        window.contentView!.addSubview(view)
        func mouse(_ type: NSEvent.EventType, _ point: NSPoint, at time: TimeInterval = 100) -> NSEvent? {
            NSEvent.mouseEvent(
                with: type, location: point, modifierFlags: [], timestamp: time, windowNumber: window.windowNumber, context: nil,
                eventNumber: 0, clickCount: 1, pressure: 1
            )
        }
        func key(_ code: UInt16) -> NSEvent? {
            NSEvent.keyEvent(
                with: .keyDown, location: .zero, modifierFlags: [], timestamp: 100, windowNumber: window.windowNumber, context: nil,
                characters: "", charactersIgnoringModifiers: "", isARepeat: false, keyCode: code
            )
        }
        let inside = NSPoint(x: 150, y: 150)
        XCTAssertTrue(BrowserKeyboard.userAsked(by: mouse(.leftMouseDown, inside), for: view, now: 100.1), "a click in it")
        XCTAssertFalse(BrowserKeyboard.userAsked(by: mouse(.leftMouseDown, NSPoint(x: 10, y: 10)), for: view, now: 100.1), "a click beside it")
        XCTAssertFalse(BrowserKeyboard.userAsked(by: mouse(.leftMouseDown, inside), for: view, now: 105), "an old click, when a page asks later")
        XCTAssertFalse(BrowserKeyboard.userAsked(by: mouse(.mouseMoved, inside), for: view, now: 100.1), "the pointer passing")
        XCTAssertTrue(BrowserKeyboard.userAsked(by: key(48), for: view, now: 100.1), "Tab")
        XCTAssertFalse(BrowserKeyboard.userAsked(by: key(0), for: view, now: 100.1), "typing in the terminal")
        XCTAssertFalse(BrowserKeyboard.userAsked(by: nil, for: view, now: 100.1), "no event: a page asking on its own")
    }

    /// A page loading in the background must not take the keyboard from the
    /// terminal beside it, where an agent may be waiting for an answer.
    func testAnAutofocusingPageLeavesTheKeyboardAlone() async throws {
        let field = NSTextField(frame: NSRect(x: 0, y: 0, width: 100, height: 20))
        window.contentView!.addSubview(field)
        window.makeKeyAndOrderFront(nil)
        window.makeFirstResponder(field)
        let editor = window.firstResponder
        let p = pane(base())
        await loaded(p, "home")
        _ = await script(p, "document.body.innerHTML = '<input autofocus id=i>'; document.getElementById('i').focus(); window.focus(); 1")
        try await Task.sleep(nanoseconds: 300_000_000)
        XCTAssertTrue(window.firstResponder === editor, "still \(String(describing: window.firstResponder))")
    }

    func testEscapeRestoresTheBar() async {
        let p = pane(base())
        window.makeKeyAndOrderFront(nil)
        await loaded(p, "home")
        p.focusLocation()
        XCTAssertTrue(p.control(NSTextField(), textView: NSTextView(), doCommandBy: #selector(NSResponder.cancelOperation(_:))))
        XCTAssertEqual(p.locationText, base())
        XCTAssertTrue(window.firstResponder === p.webView, "and hands the keyboard back to the page")
    }

    func testFocusReportsThePane() {
        let p = pane(base())
        window.makeKeyAndOrderFront(nil)
        var activations = 0
        p.onActivate = { activations += 1 }
        p.focusLocation()
        XCTAssertEqual(activations, 1, "the bar")
        p.focusContent()
        XCTAssertEqual(activations, 2, "the page")
    }

    func testABlankPaneFocusesItsBar() {
        let p = pane()
        window.makeKeyAndOrderFront(nil)
        var activations = 0
        p.onActivate = { activations += 1 }
        p.focusContent()
        XCTAssertEqual(activations, 1)
        XCTAssertFalse(window.firstResponder === p.webView)
    }
}

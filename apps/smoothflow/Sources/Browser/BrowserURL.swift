import Foundation

/// What the browser pane will load (SMOODEV-4270). Ordinary web pages only: a
/// URL bar in a console full of agent output must not become a way to read
/// local files or run script, so everything else is refused here before WebKit
/// sees it. Pure, so the rules have direct tests.
enum BrowserURL {
    enum Typed: Equatable {
        case ok(URL)
        case rejected(String)
    }

    static let webSchemes: Set<String> = ["http", "https"]
    /// Schemes that could pass for `host:port` (`javascript:1`, `file:80`). A
    /// real scheme wins, so these are never taken for a host.
    private static let knownSchemes: Set<String> = [
        "http", "https", "javascript", "vbscript", "file", "data", "blob", "about", "view-source",
        "ftp", "mailto", "ssh", "tel", "sms", "ws", "wss", "chrome", "webkit", "applewebdata",
    ]
    static let onlyWeb = "Only http and https pages open here."

    /// What the URL bar accepts: `https://…`, `http://…`, or a bare host, which
    /// gets https unless it is plainly a local one (`localhost:3000`, `smoo-hub`,
    /// `192.168.1.5`), where a dev server speaks http.
    static func parse(_ raw: String) -> Typed {
        let text = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty else { return .rejected("Type a URL.") }
        // Browsers strip tabs and newlines from URLs (`java\tscript:`), and a
        // backslash can read as a slash. None belong in something typed here.
        let unsafe = CharacterSet.whitespacesAndNewlines.union(.controlCharacters).union(CharacterSet(charactersIn: "\\"))
        guard text.rangeOfCharacter(from: unsafe) == nil else { return .rejected("That is not a URL.") }

        let explicit: Bool
        switch scheme(of: text) {
        case .web: explicit = true
        case .other: return .rejected(onlyWeb)
        case .none: explicit = false
        }
        // Paths are not hosts, and `//x` would become an empty host's path.
        if !explicit, let first = text.first, "/~.".contains(first) { return .rejected(onlyWeb) }

        guard var parts = URLComponents(string: explicit ? text : "https://\(text)"), let host = parts.host, !host.isEmpty else {
            return .rejected("That is not a URL.")
        }
        // `https://github.com@evil.example` reads as GitHub and goes to evil.example.
        guard parts.user == nil, parts.password == nil else { return .rejected("URLs with a user name or password do not open here.") }
        if !explicit, isLocal(host: host) { parts.scheme = "http" }
        guard let url = parts.url, isWeb(url) else { return .rejected("That is not a URL.") }
        return .ok(url)
    }

    private enum Scheme { case web, other, none }

    private static func scheme(of text: String) -> Scheme {
        guard let colon = text.firstIndex(of: ":") else { return .none }
        let head = text[..<colon].lowercased()
        guard let first = head.first, first.isASCII, first.isLetter,
              head.allSatisfy({ $0.isASCII && ($0.isLetter || $0.isNumber || "+-.".contains($0)) }) else { return .none }
        let rest = text[text.index(after: colon)...]
        if rest.hasPrefix("//") { return webSchemes.contains(head) ? .web : .other }
        if knownSchemes.contains(head) { return .other }
        // `localhost:3000/x` is a host and a port. `foo:bar` is some scheme.
        let port = rest.prefix { $0 != "/" && $0 != "?" && $0 != "#" }
        return !port.isEmpty && port.allSatisfy { $0.isASCII && $0.isNumber } ? .none : .other
    }

    private static func isLocal(host: String) -> Bool {
        let h = host.lowercased()
        if !h.contains(".") || h.hasSuffix(".local") || h.hasSuffix(".localhost") { return true }
        let octets = h.split(separator: ".").map { Int($0) }
        guard octets.count == 4, let a = octets[0], let b = octets[1], octets[2] != nil, octets[3] != nil else { return false }
        return a == 127 || a == 10 || a == 0 || (a == 192 && b == 168) || (a == 172 && (16 ... 31).contains(b))
    }

    /// An ordinary web page, safe to show in the top frame.
    static func isWeb(_ url: URL) -> Bool {
        guard let scheme = url.scheme?.lowercased(), webSchemes.contains(scheme), let host = url.host, !host.isEmpty else { return false }
        return url.user == nil && url.password == nil
    }

    /// May WebKit go here? Links, redirects and script all come through this. A
    /// page's own frames also need the schemes pages build frames from, which
    /// stay inside the page's sandbox; the top frame never does.
    static func allowsNavigation(_ url: URL, mainFrame: Bool) -> Bool {
        if isWeb(url) { return true }
        guard !mainFrame, let scheme = url.scheme?.lowercased() else { return false }
        return webSchemes.contains(scheme) || ["about", "data", "blob"].contains(scheme)
    }

    /// A saved URL as it may be shown again: "" stays a blank pane, and anything
    /// the bar would refuse is nil.
    static func restored(_ saved: String) -> String? {
        if saved.isEmpty { return saved }
        guard case let .ok(url) = parse(saved) else { return nil }
        return url.absoluteString
    }

    /// The tab strip's label for a browser pane.
    static func title(for url: String) -> String {
        URLComponents(string: url)?.host ?? "browser"
    }

    /// The URL a terminal link should open in a pane. Nil leaves it to
    /// `TerminalLink.perform`: mail, ssh, files, or with the setting off.
    static func paneLink(for target: TerminalLink.Target, enabled: Bool) -> URL? {
        guard enabled, case let .open(url) = target, isWeb(url) else { return nil }
        return url
    }
}

/// Settings ▸ Terminal: does a ⌘-clicked web link open here or in the default
/// browser? On unless turned off.
enum BrowserSettings {
    static let linksKey = "browser.openTerminalLinks"

    static func opensTerminalLinks(_ d: UserDefaults = .standard) -> Bool {
        d.object(forKey: linksKey) == nil ? true : d.bool(forKey: linksKey)
    }

    static func setOpensTerminalLinks(_ value: Bool, _ d: UserDefaults = .standard) { d.set(value, forKey: linksKey) }
}

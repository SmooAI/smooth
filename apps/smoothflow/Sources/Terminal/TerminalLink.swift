import AppKit
import os

/// What ⌘-click does with a link libghostty found (SMOODEV-4266). Terminal
/// output is untrusted, and an agent can print anything, so a click opens web
/// and mail links and ordinary files but never launches something: other URL
/// schemes are refused, and an app or executable is shown in Finder instead.
enum TerminalLink {
    enum Target: Equatable {
        case open(URL)
        case reveal(URL)
        case refused(String)
    }

    /// What the file system says about a path. Injected so tests need no files.
    struct Files {
        var exists: (String) -> Bool
        var isDirectory: (String) -> Bool
        var isExecutable: (String) -> Bool

        static let live = Files(
            exists: { FileManager.default.fileExists(atPath: $0) },
            isDirectory: { p in
                var dir: ObjCBool = false
                return FileManager.default.fileExists(atPath: p, isDirectory: &dir) && dir.boolValue
            },
            isExecutable: { FileManager.default.isExecutableFile(atPath: $0) }
        )
    }

    static let openableSchemes: Set<String> = ["http", "https", "mailto", "ftp", "ssh"]
    /// Bundles and documents macOS runs rather than shows when opened.
    static let launchableExtensions: Set<String> = ["app", "command", "tool", "terminal", "workflow", "action", "pkg", "mpkg", "scpt", "scptd"]

    static func resolve(_ raw: String, cwd: String?, files: Files = .live) -> Target {
        let text = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty else { return .refused(raw) }
        if let scheme = scheme(of: text) {
            if scheme == "file" {
                guard let url = URL(string: text), url.isFileURL else { return .refused(text) }
                return file(url.path, cwd: cwd, files: files) ?? .refused(text)
            }
            guard openableSchemes.contains(scheme), let url = URL(string: text) else { return .refused(text) }
            return .open(url)
        }
        return file(text, cwd: cwd, files: files) ?? .refused(text)
    }

    /// The URL scheme, or nil for a path. `foo.rs:12` has a colon and no scheme.
    private static func scheme(of text: String) -> String? {
        guard let colon = text.firstIndex(of: ":") else { return nil }
        let head = text[..<colon].lowercased()
        guard let first = head.first, first.isASCII, first.isLetter,
              head.allSatisfy({ $0.isASCII && ($0.isLetter || $0.isNumber || "+-.".contains($0)) }) else { return nil }
        let rest = text[text.index(after: colon)...]
        return rest.hasPrefix("//") || openableSchemes.contains(head) ? head : nil
    }

    private static func file(_ raw: String, cwd: String?, files: Files) -> Target? {
        let expanded = (raw as NSString).expandingTildeInPath
        var path: String
        if expanded.hasPrefix("/") {
            path = expanded
        } else if let cwd {
            path = (cwd as NSString).appendingPathComponent(expanded)
        } else {
            return nil
        }
        path = (path as NSString).standardizingPath
        if !files.exists(path) {
            // Compilers print `src/main.rs:12:3`.
            path = strippingLineSuffix(path)
            guard files.exists(path) else { return nil }
        }
        let url = URL(fileURLWithPath: path)
        if files.isDirectory(path), !launchableExtensions.contains(url.pathExtension.lowercased()) { return .open(url) }
        if launchableExtensions.contains(url.pathExtension.lowercased()) || files.isExecutable(path) { return .reveal(url) }
        return .open(url)
    }

    static func strippingLineSuffix(_ path: String) -> String {
        guard let r = path.range(of: #"(:\d+){1,2}$"#, options: .regularExpression) else { return path }
        return String(path[..<r.lowerBound])
    }

    private static let log = Logger(subsystem: "ai.smoo.smoothflow", category: "links")

    @MainActor
    static func perform(_ target: Target) {
        switch target {
        case let .open(url):
            NSWorkspace.shared.open(url)
        case let .reveal(url):
            NSWorkspace.shared.activateFileViewerSelecting([url])
        case let .refused(text):
            log.info("refused to open \(text, privacy: .private)")
            NSSound.beep()
        }
    }
}

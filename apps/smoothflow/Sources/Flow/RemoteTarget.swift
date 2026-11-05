import Foundation

/// A session on another machine, over the system `ssh` (SMOODEV-4269). The
/// engine owns the rules and refuses a bad host whatever a client sends
/// (`crates/smooth-flow/src/remote.rs`); `problem` is the same check, so a
/// sheet can say why before anything is sent.
struct RemoteTarget: Codable, Equatable {
    var host: String
    var cwd: String?

    /// How a row names it: `box`, or `box:~/src/api`.
    var label: String { cwd.map { "\(host):\($0)" } ?? host }

    var fields: [String: Any] {
        var f: [String: Any] = ["host": host]
        if let cwd { f["cwd"] = cwd }
        return f
    }

    /// What the New Session sheets and the fleet row's tooltip say happens to
    /// a remote session. Keep in step with SmoothFlow.md § Remote sessions.
    static let survivalNote = """
        Runs through your system ssh, using your ~/.ssh/config and agent. A password or 2FA prompt appears in the session. \
        Quitting SmoothFlow keeps the connection open. If the connection drops (a network change, a long sleep), \
        the remote program usually ends with it, unless you started it under tmux or screen on that machine. A lost shell stays closed; \
        a lost agent waits for you to resume its conversation. After this Mac restarts, shells reconnect with a new shell and agents wait the same way.
        """

    /// Why `host` can't be used, or nil when it can. Blank is "no host yet".
    static func problem(_ host: String) -> String? {
        let host = host.trimmingCharacters(in: .whitespacesAndNewlines)
        if host.isEmpty { return "Enter a host" }
        if host.utf8.count > 255 { return "That host is too long" }
        let parts = host.components(separatedBy: "@")
        let name = parts.last ?? host
        if host.hasPrefix("-") || name.hasPrefix("-") { return "A host can't start with “-”: ssh would read it as an option" }
        let allowed = CharacterSet(charactersIn: "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789._-@:%")
        if host.unicodeScalars.contains(where: { !allowed.contains($0) }) { return "Use [user@]host, or a Host alias from ~/.ssh/config" }
        if name.isEmpty || parts.count > 2 || parts[0].isEmpty { return "Use [user@]host, or a Host alias from ~/.ssh/config" }
        if parts.count == 2, parts[0].contains(":") { return "Don't put a password in the host. Use an ssh agent, or answer the prompt in the session" }
        return nil
    }

    /// A target from what the sheet holds, or nil when the host is refused.
    static func from(host: String, cwd: String) -> RemoteTarget? {
        guard problem(host) == nil else { return nil }
        let dir = cwd.trimmingCharacters(in: .whitespacesAndNewlines)
        return RemoteTarget(host: host.trimmingCharacters(in: .whitespacesAndNewlines), cwd: dir.isEmpty ? nil : dir)
    }
}

/// The hosts you opened sessions on, newest first, offered in the sheets. Only
/// host names are kept. A value `RemoteTarget.problem` refuses, which covers
/// anything carrying a password, is never stored.
enum RecentHosts {
    static let key = "recentRemoteHosts"
    static let limit = 8

    static func load(_ d: UserDefaults = .standard) -> [String] { d.stringArray(forKey: key) ?? [] }

    static func remember(_ host: String, _ d: UserDefaults = .standard) {
        guard RemoteTarget.problem(host) == nil else { return }
        let h = host.trimmingCharacters(in: .whitespacesAndNewlines)
        d.set(Array(([h] + load(d).filter { $0 != h }).prefix(limit)), forKey: key)
    }
}

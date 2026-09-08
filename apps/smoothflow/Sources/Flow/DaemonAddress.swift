import Foundation

/// Where the flow engine lives and which binary to launch. Pure functions over
/// explicit inputs so they are unit-testable without touching the real HOME.
enum DaemonAddress {
    struct Endpoint: Equatable {
        var host: String
        var port: Int
        /// The daemon's local token (`?token=` on the WS, `X-Smooth-Token` on HTTP).
        /// Every flow route but `/hooks` is gated on it.
        var token: String? = nil
        var wsURL: URL {
            var c = URLComponents(string: "ws://\(host):\(port)/api/flow/ws")!
            if let token { c.queryItems = [URLQueryItem(name: "token", value: token)] }
            return c.url!
        }
        var httpBase: URL { URL(string: "http://\(host):\(port)/")! }
        var description: String { "\(host):\(port)" }
    }

    enum Resolution: Equatable {
        /// Connect to a daemon we did not start (env / setting / mock server).
        case external(Endpoint)
        /// Start our own child (or LaunchAgent) — the default. Never a
        /// terminal-launched daemon: TCC attribution would be wrong.
        case spawn
    }

    static let envKey = "SMOOTHFLOW_DAEMON_ADDR"
    static let defaultsKey = "daemonAddr"
    static let binaryEnvKey = "SMOOTHFLOW_DAEMON_BIN"
    static let binaryDefaultsKey = "daemonBinary"
    static let tokenEnvKey = "SMOOTHFLOW_DAEMON_TOKEN"

    /// Order: env override → Settings override → spawn. `~/.smooth/daemon.addr`
    /// is deliberately NOT consulted: it advertises whatever daemon happened to
    /// start last, terminal ones included.
    static func resolve(env: [String: String], setting: String?) -> Resolution {
        if let e = env[envKey].flatMap(parse) { return .external(e) }
        if let s = setting.flatMap(parse) { return .external(s) }
        return .spawn
    }

    /// `host:port`, `http://host:port`, `ws://host:port/...` or a bare port.
    static func parse(_ raw: String) -> Endpoint? {
        let s = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !s.isEmpty else { return nil }
        if let p = Int(s), (1...65535).contains(p) { return Endpoint(host: "127.0.0.1", port: p) }
        var body = s
        for scheme in ["ws://", "wss://", "http://", "https://"] where body.hasPrefix(scheme) { body = String(body.dropFirst(scheme.count)) }
        if let slash = body.firstIndex(of: "/") { body = String(body[..<slash]) }
        guard let colon = body.lastIndex(of: ":"), let port = Int(body[body.index(after: colon)...]), (1...65535).contains(port) else { return nil }
        let host = String(body[..<colon])
        return Endpoint(host: host.isEmpty ? "127.0.0.1" : host, port: port)
    }

    /// The `smooth-daemon` to launch: explicit override (`SMOOTHFLOW_DAEMON_BIN`
    /// / Settings) → bundled → `~/.cargo/bin` → PATH. The override is how a dev
    /// build points the app at an engine built elsewhere.
    static func daemonBinary(override: String? = nil, bundleExecutableDir: URL?, home: URL, path: String, exists: (String) -> Bool) -> String? {
        var candidates: [String] = []
        if let o = override?.trimmingCharacters(in: .whitespacesAndNewlines), !o.isEmpty { candidates.append((o as NSString).expandingTildeInPath) }
        if let dir = bundleExecutableDir { candidates.append(dir.appendingPathComponent("smooth-daemon").path) }
        candidates.append(home.appendingPathComponent(".cargo/bin/smooth-daemon").path)
        candidates += path.split(separator: ":").map { "\($0)/smooth-daemon" }
        return candidates.first(where: exists)
    }

    /// `~/.smooth/daemon.addr` as written by the daemon (host:port, one line).
    static func readAddrFile(_ contents: String) -> Endpoint? { parse(contents) }

    /// The local token to present: `SMOOTHFLOW_DAEMON_TOKEN` → `SMOOTH_LOCAL_TOKEN`
    /// → `~/.smooth/operator-token` (what the daemon itself provisions). Blank
    /// values are unset. `nil` means an open daemon (or one we cannot talk to).
    static func token(env: [String: String], tokenFile: String?) -> String? {
        for k in [tokenEnvKey, "SMOOTH_LOCAL_TOKEN"] {
            if let t = env[k]?.trimmingCharacters(in: .whitespacesAndNewlines), !t.isEmpty { return t }
        }
        if let t = tokenFile?.trimmingCharacters(in: .whitespacesAndNewlines), !t.isEmpty { return t }
        return nil
    }
}

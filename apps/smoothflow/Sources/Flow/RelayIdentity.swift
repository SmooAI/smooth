import Foundation

/// The child daemon's identity on the Smoo Relay (pearl th-a1bb12).
///
/// Big Smooth's daemon registers on `relay.smoo.ai` as the machine's
/// `~/.smooth/relay-device-id`. The SmoothFlow child used to read the SAME file,
/// so both daemons dialed in as one device and every phone landed on whichever
/// connected last. The app therefore mints its own id once, keeps it in
/// `~/.smooth/smoothflow-relay-device-id`, and hands the child
/// `SMOOTH_RELAY_DEVICE_ID` + `SMOOTH_RELAY_LABEL` + `SMOOTH_RELAY_KIND=flow`, so
/// the relay's device list shows "smoo-hub" (Big Smooth) and
/// "smoo-hub · SmoothFlow" as two peers. Pure over explicit inputs so it is
/// unit-testable without touching the real HOME.
enum RelayIdentity {
    static let deviceIdFile = ".smooth/smoothflow-relay-device-id"
    /// The relay's `?kind=` for a flow-only daemon (`rust/relay-ws` accepts
    /// `daemon` | `flow` | `phone`, SMOODEV-3142).
    static let kind = "flow"
    static let labelSuffix = " · SmoothFlow"
    /// The relay's device-id grammar (`rust/relay-ws` `valid_device`): 1–64 of
    /// `[A-Za-z0-9._-]`. A persisted id that fails it is replaced, never sent.
    static let maxIdLength = 64

    /// The id to use: the persisted one when it is well-formed, else a fresh
    /// `daemon-<12 hex>` (the same shape the daemon mints for itself, so the
    /// relay and the phones treat both daemons alike). `fresh` says whether the
    /// caller must persist it.
    static func deviceId(persisted: String?, mint: () -> String = mintDeviceId) -> (id: String, fresh: Bool) {
        if let p = persisted?.trimmingCharacters(in: .whitespacesAndNewlines), isValidDeviceId(p) { return (p, false) }
        return (mint(), true)
    }

    static func mintDeviceId() -> String {
        "daemon-" + String(UUID().uuidString.lowercased().replacingOccurrences(of: "-", with: "").prefix(12))
    }

    static func isValidDeviceId(_ s: String) -> Bool {
        !s.isEmpty && s.count <= maxIdLength && s.allSatisfy { $0.isASCII && ($0.isLetter || $0.isNumber || $0 == "." || $0 == "_" || $0 == "-") }
    }

    /// `<host> · SmoothFlow` from the machine's short hostname (`smoo-hub.local`
    /// → `smoo-hub`); a blank hostname becomes `big-smooth · SmoothFlow`, the
    /// daemon's own fallback.
    static func label(hostname: String?) -> String {
        let short = (hostname ?? "").trimmingCharacters(in: .whitespacesAndNewlines).split(separator: ".").first.map(String.init) ?? ""
        let host = short.unicodeScalars.filter { !CharacterSet.controlCharacters.contains($0) }.map { String($0) }.joined()
        return (host.isEmpty ? "big-smooth" : host) + labelSuffix
    }

    /// The three relay variables the child is started with.
    static func environment(deviceId: String, hostname: String?) -> [String: String] {
        ["SMOOTH_RELAY_DEVICE_ID": deviceId, "SMOOTH_RELAY_LABEL": label(hostname: hostname), "SMOOTH_RELAY_KIND": kind]
    }

    /// Read-or-mint against a real home directory (mode 600 on first write,
    /// like the daemon's own `relay-device-id`). A home that cannot be written
    /// still yields an id — it just changes on the next launch, which the
    /// daemon also tolerates.
    static func load(home: URL) -> String {
        let url = home.appendingPathComponent(deviceIdFile)
        let persisted = try? String(contentsOf: url, encoding: .utf8)
        let (id, fresh) = deviceId(persisted: persisted)
        if fresh {
            try? FileManager.default.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
            try? (id + "\n").write(to: url, atomically: true, encoding: .utf8)
            try? FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: url.path)
        }
        return id
    }
}

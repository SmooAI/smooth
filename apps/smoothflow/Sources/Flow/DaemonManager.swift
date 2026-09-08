import Foundation
import ServiceManagement

/// Owns the `smooth-daemon` lifecycle so every TCC grant attributes to THIS
/// app: the daemon is a child of the app process (default) or a LaunchAgent
/// registered from inside the bundle (`SMAppService`). A daemon someone started
/// from a terminal is never used — its prompts would be silently denied.
@MainActor
final class DaemonManager: ObservableObject {
    enum Mode: String { case child, launchAgent }

    @Published private(set) var status: String = "not started"
    @Published private(set) var endpoint: DaemonAddress.Endpoint?
    @Published private(set) var binary: String?

    static let modeKey = "daemonMode"
    static let agentPlist = "ai.smoo.smoothflow.daemon.plist"
    static let agentPort = 8791

    private var process: Process?
    private var stopping = false
    private var restartDelay: TimeInterval = 1
    private var log: FileHandle?
    private let home = FileManager.default.homeDirectoryForCurrentUser

    var mode: Mode {
        get { Mode(rawValue: UserDefaults.standard.string(forKey: Self.modeKey) ?? "") ?? .child }
        set { UserDefaults.standard.set(newValue.rawValue, forKey: Self.modeKey) }
    }

    /// LaunchAgent mode needs the daemon inside the bundle (the plist's
    /// BundleProgram is relative to it); a dev build falls back to child mode.
    var launchAgentAvailable: Bool {
        let dir = Bundle.main.executableURL?.deletingLastPathComponent()
        return dir.map { FileManager.default.fileExists(atPath: $0.appendingPathComponent("smooth-daemon").path) } ?? false
    }

    func resolveBinary() -> String? {
        DaemonAddress.daemonBinary(bundleExecutableDir: Bundle.main.executableURL?.deletingLastPathComponent(),
                                   home: home,
                                   path: ProcessInfo.processInfo.environment["PATH"] ?? "/usr/local/bin:/opt/homebrew/bin:/usr/bin",
                                   exists: { FileManager.default.isExecutableFile(atPath: $0) })
    }

    /// Start (or adopt) the daemon and hand back where to connect.
    func start() -> DaemonAddress.Endpoint? {
        stopping = false
        binary = resolveBinary()
        guard let binary else {
            status = "smooth-daemon not found (bundle, ~/.cargo/bin, PATH)"
            return nil
        }
        switch mode {
        case .launchAgent where launchAgentAvailable:
            return startLaunchAgent()
        default:
            return startChild(binary)
        }
    }

    func stop() {
        stopping = true
        if let p = process, p.isRunning {
            p.terminate()
            p.waitUntilExit()
        }
        process = nil
        status = "stopped"
    }

    // MARK: child mode

    private func startChild(_ binary: String) -> DaemonAddress.Endpoint? {
        let port = Self.freePort()
        let ep = DaemonAddress.Endpoint(host: "127.0.0.1", port: port)
        let p = Process()
        p.executableURL = URL(fileURLWithPath: binary)
        p.arguments = ["operator", "--addr", ep.description]
        var env = ProcessInfo.processInfo.environment
        env["SMOOTHFLOW_PARENT"] = Bundle.main.bundleIdentifier ?? "ai.smoo.smoothflow"
        // The daemon refuses to start next to a running Big Smooth (they would
        // share operator-storage.db). SmoothFlow's daemon is a separate product
        // on its own port; opt out of the guard. ponytail: flow.db is separate,
        // operator-storage.db is still shared — split it when the engine lands.
        env["SMOOTH_ALLOW_SECOND_DAEMON"] = "1"
        // A GUI app's PATH is tiny; agents the daemon launches need the usual dirs.
        env["PATH"] = ["\(home.path)/.cargo/bin", "/opt/homebrew/bin", "/usr/local/bin", env["PATH"] ?? "/usr/bin:/bin"].joined(separator: ":")
        p.environment = env
        let logURL = home.appendingPathComponent(".smooth/smoothflow-daemon.log")
        try? FileManager.default.createDirectory(at: logURL.deletingLastPathComponent(), withIntermediateDirectories: true)
        FileManager.default.createFile(atPath: logURL.path, contents: nil)
        if let h = try? FileHandle(forWritingTo: logURL) {
            h.seekToEndOfFile()
            log = h
            p.standardOutput = h
            p.standardError = h
        }
        p.terminationHandler = { [weak self] proc in
            Task { @MainActor in self?.childExited(code: proc.terminationStatus) }
        }
        do {
            try p.run()
        } catch {
            status = "launch failed: \(error.localizedDescription)"
            return nil
        }
        process = p
        endpoint = ep
        status = "child pid \(p.processIdentifier) on \(ep.description)"
        return ep
    }

    private func childExited(code: Int32) {
        process = nil
        guard !stopping else { return }
        status = "daemon exited (\(code)); restarting in \(Int(restartDelay))s"
        let delay = restartDelay
        restartDelay = min(restartDelay * 2, 30)
        Task { @MainActor [weak self] in
            try? await Task.sleep(nanoseconds: UInt64(delay * 1_000_000_000))
            guard let self, !self.stopping, let binary = self.binary else { return }
            if let ep = self.startChild(binary) { self.onRestart?(ep) }
        }
    }

    /// The app reconnects its flow client here after a crash-restart.
    var onRestart: ((DaemonAddress.Endpoint) -> Void)?

    // MARK: LaunchAgent mode

    private func startLaunchAgent() -> DaemonAddress.Endpoint? {
        let service = SMAppService.agent(plistName: Self.agentPlist)
        do {
            if service.status != .enabled { try service.register() }
            status = "LaunchAgent \(service.status == .enabled ? "enabled" : "pending approval (System Settings ▸ Login Items)")"
        } catch {
            status = "LaunchAgent registration failed: \(error.localizedDescription)"
            return nil
        }
        let ep = DaemonAddress.Endpoint(host: "127.0.0.1", port: Self.agentPort)
        endpoint = ep
        return ep
    }

    func unregisterLaunchAgent() {
        try? SMAppService.agent(plistName: Self.agentPlist).unregister()
    }

    // MARK: tmux server

    /// Start the `smoothflow` tmux server as a DIRECT child of the app so the
    /// panes agents run in carry the app's TCC attribution even when the daemon
    /// restarts. Measured (docs/Architecture/SmoothFlow-macOS.md): attribution
    /// survives tmux daemonizing and any depth of children, but is LOST the
    /// moment the app exits — a surviving server re-attributes to itself — so a
    /// server we did not start this launch is killed and recreated.
    @discardableResult
    func startTmuxServer() -> Bool {
        guard let tmux = Self.tmuxBinary else { return false }
        _ = Self.tmux(tmux, ["kill-server"])
        return Self.tmux(tmux, ["new-session", "-d", "-s", "_smoothflow", "-x", "200", "-y", "50", "exec sleep 2147483647"])
    }

    /// Quitting the app ends the fleet's TCC attribution, so end the fleet too;
    /// the engine resumes agents (`--resume`) on the next launch.
    func stopTmuxServer() {
        if let tmux = Self.tmuxBinary { _ = Self.tmux(tmux, ["kill-server"]) }
    }

    static let tmuxBinary = ["/opt/homebrew/bin/tmux", "/usr/local/bin/tmux", "/usr/bin/tmux"].first { FileManager.default.isExecutableFile(atPath: $0) }

    private static func tmux(_ bin: String, _ args: [String]) -> Bool {
        let p = Process()
        p.executableURL = URL(fileURLWithPath: bin)
        p.arguments = ["-L", "smoothflow"] + args
        p.standardOutput = FileHandle.nullDevice
        p.standardError = FileHandle.nullDevice
        do { try p.run(); p.waitUntilExit(); return p.terminationStatus == 0 } catch { return false }
    }

    // MARK: helpers

    static func freePort() -> Int {
        let sock = socket(AF_INET, SOCK_STREAM, 0)
        defer { close(sock) }
        var addr = sockaddr_in()
        addr.sin_family = sa_family_t(AF_INET)
        addr.sin_addr.s_addr = inet_addr("127.0.0.1")
        addr.sin_port = 0
        var len = socklen_t(MemoryLayout<sockaddr_in>.size)
        let bound = withUnsafeMutablePointer(to: &addr) { $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { bind(sock, $0, len) } }
        guard bound == 0 else { return 8790 }
        _ = withUnsafeMutablePointer(to: &addr) { $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { getsockname(sock, $0, &len) } }
        return Int(UInt16(bigEndian: addr.sin_port))
    }
}

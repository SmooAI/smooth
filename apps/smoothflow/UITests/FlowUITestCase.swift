import AppKit
import Foundation
import XCTest

/// Shared harness for the SmoothFlow UI tests (pearl th-a58a97).
///
/// Two backends, both started by the test and torn down with it:
///   * `startMock()`   — `mock/server.mjs` on a free port (the wireframe fleet).
///   * `startEngine()` — the real flow engine (`flow_e2e_server`, same router +
///     supervisor as smooth-daemon) with `tests/fixtures/fake-claude` on PATH as
///     `claude`, a throwaway HOME, its own flow.db and a private tmux socket.
///
/// The app is launched with `SMOOTHFLOW_DAEMON_ADDR` (external mode: it never
/// spawns a daemon), `SMOOTHFLOW_UI_TEST=1` (no Sparkle) and `CFFIXED_USER_HOME`
/// + `HOME` pointed at a temp dir, so UserDefaults, `~/.smooth/*` reads and the
/// onboarding flag never touch the developer's real home. No fixed sleeps:
/// every assertion polls with a timeout.
class FlowUITestCase: XCTestCase {
    static let appDir = URL(fileURLWithPath: #filePath).deletingLastPathComponent().deletingLastPathComponent()
    static let repoRoot = appDir.deletingLastPathComponent().deletingLastPathComponent()
    static let fakeClaude = repoRoot.appendingPathComponent("crates/smooth-daemon/tests/fixtures/fake-claude")

    var app: XCUIApplication!
    var tmp: URL!
    var addr = ""
    var token: String?
    private var processes: [Process] = []
    private var tmuxSocket: String?

    override func setUpWithError() throws {
        try super.setUpWithError()
        continueAfterFailure = false
        tmp = FileManager.default.temporaryDirectory.appendingPathComponent("smoothflow-ui-\(UUID().uuidString.prefix(8))")
        try FileManager.default.createDirectory(at: tmp.appendingPathComponent("home/.smooth"), withIntermediateDirectories: true)
    }

    override func tearDown() {
        if let run = testRun, run.totalFailureCount > 0, let app {
            let shot = XCTAttachment(screenshot: app.screenshot())
            shot.name = "\(name)-failure"
            shot.lifetime = .keepAlways
            add(shot)
        }
        app?.terminate()
        for p in processes where p.isRunning { p.terminate() }
        if let sock = tmuxSocket, let tmux = Self.which("tmux") {
            _ = try? Process.run(URL(fileURLWithPath: tmux), arguments: ["-L", sock, "kill-server"])
        }
        if let tmp { try? FileManager.default.removeItem(at: tmp) }
        super.tearDown()
    }

    // MARK: backends

    /// The wireframe fleet from mock/server.mjs. Skips without node.
    func startMock() throws {
        guard let node = Self.which("node") else { throw XCTSkip("node not installed (set SMOOTHFLOW_NODE)") }
        let port = Self.freePort()
        let p = Process()
        p.executableURL = URL(fileURLWithPath: node)
        p.arguments = [Self.appDir.appendingPathComponent("mock/server.mjs").path, String(port)]
        p.standardOutput = FileHandle.nullDevice
        try p.run()
        processes.append(p)
        addr = "127.0.0.1:\(port)"
        token = nil
        try waitForHTTP()
    }

    /// The real engine + fake-claude. Skips when tmux or the server binary is absent.
    /// `SMOOTHFLOW_E2E_SERVER` overrides the binary (CI points it at the cargo build).
    func startEngine() throws {
        guard Self.which("tmux") != nil else { throw XCTSkip("tmux not installed") }
        let env = ProcessInfo.processInfo.environment
        let server = env["SMOOTHFLOW_E2E_SERVER"]
            ?? "\(env["HOME"] ?? NSHomeDirectory())/.cargo/target-e2e/debug/examples/flow_e2e_server"
        guard FileManager.default.isExecutableFile(atPath: server) else {
            throw XCTSkip("flow_e2e_server not built (\(server)); cargo build -p smooai-smooth-daemon --example flow_e2e_server")
        }
        let fm = FileManager.default
        let bin = tmp.appendingPathComponent("bin"), ws = tmp.appendingPathComponent("ws")
        try fm.createDirectory(at: bin, withIntermediateDirectories: true)
        try fm.createDirectory(at: ws, withIntermediateDirectories: true)
        try fm.copyItem(at: Self.fakeClaude, to: bin.appendingPathComponent("claude"))
        try fm.setAttributes([.posixPermissions: 0o755], ofItemAtPath: bin.appendingPathComponent("claude").path)
        let sock = "flow-ui-\(ProcessInfo.processInfo.processIdentifier)"
        tmuxSocket = sock
        token = "e2e-tok"
        let p = Process()
        p.executableURL = URL(fileURLWithPath: server)
        p.arguments = ["--addr", "127.0.0.1:0", "--workspace", ws.path, "--db", ws.appendingPathComponent("flow.db").path, "--token", token!, "--tmux-socket", sock]
        p.environment = env.merging(["PATH": "\(bin.path):\(env["PATH"] ?? "/usr/bin:/bin")", "HOME": tmp.appendingPathComponent("home").path]) { $1 }
        p.standardOutput = FileHandle.nullDevice
        try p.run()
        processes.append(p)
        // The server writes its bound address next to the workspace (what fake-claude reads too).
        let addrFile = ws.appendingPathComponent(".flow-e2e-addr")
        let deadline = Date().addingTimeInterval(30)
        while Date() < deadline {
            if let a = try? String(contentsOf: addrFile, encoding: .utf8).trimmingCharacters(in: .whitespacesAndNewlines), !a.isEmpty {
                addr = a
                try waitForHTTP()
                return
            }
            RunLoop.current.run(until: Date().addingTimeInterval(0.1))
        }
        XCTFail("flow_e2e_server never wrote \(addrFile.path)")
    }

    // MARK: app

    func launchApp() throws {
        // XCUIApplication.launch() kills any running instance of the bundle id — that
        // would be the developer's real SmoothFlow.app (and its daemon's TCC lineage).
        // Refuse rather than take it down; CI has no such instance.
        if !NSRunningApplication.runningApplications(withBundleIdentifier: "ai.smoo.smoothflow").isEmpty {
            throw XCTSkip("SmoothFlow.app is running — quit it before running the UI tests")
        }
        let a = XCUIApplication()
        var env = ["SMOOTHFLOW_DAEMON_ADDR": addr, "SMOOTHFLOW_UI_TEST": "1",
                   "HOME": tmp.appendingPathComponent("home").path, "CFFIXED_USER_HOME": tmp.appendingPathComponent("home").path]
        if let token { env["SMOOTHFLOW_DAEMON_TOKEN"] = token }
        a.launchEnvironment = env
        a.launchArguments = ["-onboarded", "YES", "-ApplePersistenceIgnoreState", "YES", "-NSQuitAlwaysKeepsWindows", "NO"]
        a.launch()
        app = a
        XCTAssertTrue(app.windows["SmoothFlow"].waitForExistence(timeout: 20), "main window")
        XCTAssertTrue(waitUntil(timeout: 20) { self.label("sidebar.connection").hasPrefix("daemon connected") },
                      "connected to \(addr): connection='\(label("sidebar.connection"))'")
    }

    // MARK: waiting

    /// Poll `cond` on the main run loop until it holds or `timeout` passes.
    @discardableResult
    func waitUntil(timeout: TimeInterval = 15, _ cond: () -> Bool) -> Bool {
        let deadline = Date().addingTimeInterval(timeout)
        while Date() < deadline {
            if cond() { return true }
            RunLoop.current.run(until: Date().addingTimeInterval(0.2))
        }
        return cond()
    }

    /// The text of a static text. On macOS an AXStaticText carries its string in
    /// `value`, not `label` (label is the AXDescription, usually empty).
    func label(_ id: String) -> String {
        let e = app.staticTexts[id]
        guard e.exists else { return "" }
        return (e.value as? String).flatMap { $0.isEmpty ? nil : $0 } ?? e.label
    }

    func waitForState(_ sessionId: String, timeout: TimeInterval = 20, _ predicate: @escaping (String) -> Bool) {
        let ok = waitUntil(timeout: timeout) { predicate(self.label("sidebar.state.\(sessionId)")) }
        XCTAssertTrue(ok, "sidebar state for \(sessionId) is '\(label("sidebar.state.\(sessionId)"))'")
    }

    // MARK: HTTP (the test's own view of the backend)

    @discardableResult
    func http(_ method: String, _ path: String, body: [String: Any]? = nil) -> (status: Int, text: String) {
        var req = URLRequest(url: URL(string: "http://\(addr)\(path)")!)
        req.httpMethod = method
        req.timeoutInterval = 10
        if let token { req.setValue(token, forHTTPHeaderField: "X-Smooth-Token") }
        if let body {
            req.setValue("application/json", forHTTPHeaderField: "Content-Type")
            req.httpBody = try? JSONSerialization.data(withJSONObject: body)
        }
        var out = (status: 0, text: "")
        let done = DispatchSemaphore(value: 0)
        URLSession.shared.dataTask(with: req) { data, resp, _ in
            out = ((resp as? HTTPURLResponse)?.statusCode ?? 0, data.flatMap { String(data: $0, encoding: .utf8) } ?? "")
            done.signal()
        }.resume()
        _ = done.wait(timeout: .now() + 15)
        return out
    }

    /// `POST /api/flow/sessions` → the new session id.
    func newSession(_ body: [String: Any] = ["kind": "claude"]) throws -> String {
        let r = http("POST", "/api/flow/sessions", body: body)
        XCTAssertEqual(r.status, 200, r.text)
        let json = try XCTUnwrap(try JSONSerialization.jsonObject(with: Data(r.text.utf8)) as? [String: Any])
        return try XCTUnwrap((json["session"] as? [String: Any])?["id"] as? String, r.text)
    }

    /// Poll the visible pane (`GET …/snapshot`) until it contains `needle`.
    func waitForSnapshot(_ id: String, containing needle: String, timeout: TimeInterval = 30) {
        var last = ""
        let ok = waitUntil(timeout: timeout) {
            last = self.http("GET", "/api/flow/sessions/\(id)/snapshot").text
            return last.contains(needle)
        }
        XCTAssertTrue(ok, "pane of \(id) never showed \(needle); last: \(last.suffix(400))")
    }

    private func waitForHTTP() throws {
        // A 401 is also "up": the engine gates /sessions on the token, the mock does not.
        let ok = waitUntil(timeout: 30) { [self] in
            let s = http("GET", "/api/flow/sessions").status
            return s == 200 || s == 401
        }
        if !ok { throw XCTSkip("backend at \(addr) never answered") }
    }

    // MARK: process helpers

    /// `SMOOTHFLOW_<NAME>` (e.g. `SMOOTHFLOW_NODE=$(command -v node)`) wins; then the
    /// usual dirs + mise shims, since xcodebuild's PATH rarely has a version manager on it.
    static func which(_ name: String) -> String? {
        let env = ProcessInfo.processInfo.environment
        if let o = env["SMOOTHFLOW_\(name.uppercased())"], FileManager.default.isExecutableFile(atPath: o) { return o }
        let home = env["HOME"] ?? NSHomeDirectory()
        // A real mise install beats its shim: the shim re-resolves through mise config, which a spawned process may not see.
        let mise = "\(home)/.local/share/mise/installs/\(name)"
        let installs = ((try? FileManager.default.contentsOfDirectory(atPath: mise)) ?? []).sorted().reversed().map { "\(mise)/\($0)/bin" }
        let dirs = ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/bin"] + installs + ["\(home)/.local/share/mise/shims"] + (env["PATH"] ?? "").split(separator: ":").map(String.init)
        return dirs.map { "\($0)/\(name)" }.first { FileManager.default.isExecutableFile(atPath: $0) }
    }

    static func freePort() -> Int {
        let fd = socket(AF_INET, SOCK_STREAM, 0)
        defer { close(fd) }
        var a = sockaddr_in(sin_len: UInt8(MemoryLayout<sockaddr_in>.size), sin_family: sa_family_t(AF_INET), sin_port: 0, sin_addr: in_addr(s_addr: inet_addr("127.0.0.1")), sin_zero: (0, 0, 0, 0, 0, 0, 0, 0))
        var len = socklen_t(MemoryLayout<sockaddr_in>.size)
        let bound = withUnsafeMutablePointer(to: &a) { $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { Darwin.bind(fd, $0, len) == 0 && Darwin.getsockname(fd, $0, &len) == 0 } }
        return bound ? Int(UInt16(bigEndian: a.sin_port)) : 8790
    }
}

import Foundation

/// The flow WebSocket (`/api/flow/ws`) plus the one-shot HTTP siblings.
/// Reconnects with backoff; every decoded frame goes through `store.apply`
/// and the resulting effects to `onEffects`.
@MainActor
final class FlowClient {
    let store: FlowStore
    var onEffects: ([StoreEffect]) -> Void = { _ in }

    private(set) var address: DaemonAddress.Endpoint?
    private var task: URLSessionWebSocketTask?
    private var session: URLSession = .shared
    private var backoff: TimeInterval = 1
    private var closed = false
    private var reconnectTimer: Timer?

    init(store: FlowStore) { self.store = store }

    static let version = Bundle.main.infoDictionary?["CFBundleShortVersionString"] as? String ?? "dev"

    func connect(to endpoint: DaemonAddress.Endpoint) {
        address = endpoint
        closed = false
        backoff = 1
        open()
    }

    func disconnect() {
        closed = true
        reconnectTimer?.invalidate()
        task?.cancel(with: .goingAway, reason: nil)
        task = nil
        store.connection = .disconnected(reason: nil)
    }

    /// `seq` rides on the frame so the engine's reply (`flow.error.ref`) can be
    /// matched to it; most frames need none.
    func send(_ frame: ClientFrame, seq: Int? = nil) {
        guard let task, store.connection.isConnected else { return }
        // Text, not binary: the engine's WS loop only reads `Message::Text`.
        task.send(.string(frame.encodeText(seq: seq))) { [weak self] error in
            if let error { Task { @MainActor in self?.dropped("send failed: \(error.localizedDescription)") } }
        }
    }

    /// `GET /api/flow/sessions/{id}/handoff`
    func handoff(for id: String) async throws -> Handoff {
        guard let address else { throw URLError(.cannotConnectToHost) }
        let url = address.httpBase.appendingPathComponent("api/flow/sessions/\(id)/handoff")
        var req = URLRequest(url: url)
        if let t = address.token { req.setValue(t, forHTTPHeaderField: "X-Smooth-Token") }
        let (data, resp) = try await session.data(for: req)
        if let code = (resp as? HTTPURLResponse)?.statusCode, code != 200 { throw URLError(code == 401 ? .userAuthenticationRequired : .badServerResponse) }
        return try JSONDecoder().decode(Handoff.self, from: data)
    }

    /// `GET /api/flow/infer?cwd=…` — the New Session dialog's read-only
    /// context. `nil` infers from the daemon's own workspace (th-c103c1).
    func infer(cwd: String?) async throws -> InferredContext {
        guard let address else { throw URLError(.cannotConnectToHost) }
        var comps = URLComponents(url: address.httpBase.appendingPathComponent("api/flow/infer"), resolvingAgainstBaseURL: false)
        if let cwd, !cwd.isEmpty { comps?.queryItems = [URLQueryItem(name: "cwd", value: cwd)] }
        guard let url = comps?.url else { throw URLError(.badURL) }
        var req = URLRequest(url: url)
        if let t = address.token { req.setValue(t, forHTTPHeaderField: "X-Smooth-Token") }
        let (data, resp) = try await session.data(for: req)
        if let code = (resp as? HTTPURLResponse)?.statusCode, code != 200 { throw URLError(code == 401 ? .userAuthenticationRequired : .badServerResponse) }
        return try JSONDecoder().decode(InferredContext.self, from: data)
    }

    /// `GET /api/flow/harnesses` — every manifest, hidden ones flagged (Settings).
    func allHarnesses() async throws -> [HarnessInfo] {
        try await harnessCall(method: "GET", path: "api/flow/harnesses", body: nil)
    }

    /// `PUT /api/flow/harnesses/prefs {order?, hidden?}` — returns the full list.
    func putHarnessPrefs(order: [String]?, hidden: [String]?) async throws -> [HarnessInfo] {
        var obj: [String: Any] = [:]
        if let order { obj["order"] = order }
        if let hidden { obj["hidden"] = hidden }
        return try await harnessCall(method: "PUT", path: "api/flow/harnesses/prefs", body: try JSONSerialization.data(withJSONObject: obj))
    }

    private struct HarnessList: Decodable { var harnesses: [HarnessInfo] }

    // ── phone pairing (th-d98fde) ────────────────────────────────────────────

    /// `POST /api/flow/pair` — mint a QR.
    func beginPairing() async throws -> PairingBegin {
        try await jsonCall(method: "POST", path: "api/flow/pair")
    }

    /// `GET /api/flow/pair/{id}` — poll until scanned.
    func pairingStatus(_ id: String) async throws -> PairingPoll {
        try await jsonCall(method: "GET", path: "api/flow/pair/\(id)")
    }

    /// `GET /api/flow/pairings`
    func pairings() async throws -> PairingsList {
        try await jsonCall(method: "GET", path: "api/flow/pairings")
    }

    /// `DELETE /api/flow/pairings/{device}` → whether a pairing was removed.
    func revokePairing(_ device: String) async throws -> Bool {
        struct Reply: Decodable { var revoked: Bool }
        let r: Reply = try await jsonCall(method: "DELETE", path: "api/flow/pairings/\(device)")
        return r.revoked
    }

    private func jsonCall<T: Decodable>(method: String, path: String) async throws -> T {
        guard let address else { throw URLError(.cannotConnectToHost) }
        var req = URLRequest(url: address.httpBase.appendingPathComponent(path))
        req.httpMethod = method
        if let t = address.token { req.setValue(t, forHTTPHeaderField: "X-Smooth-Token") }
        let (data, resp) = try await session.data(for: req)
        if let code = (resp as? HTTPURLResponse)?.statusCode, code != 200 { throw URLError(code == 401 ? .userAuthenticationRequired : .badServerResponse) }
        return try JSONDecoder().decode(T.self, from: data)
    }

    private func harnessCall(method: String, path: String, body: Data?) async throws -> [HarnessInfo] {
        guard let address else { throw URLError(.cannotConnectToHost) }
        var req = URLRequest(url: address.httpBase.appendingPathComponent(path))
        req.httpMethod = method
        req.httpBody = body
        if body != nil { req.setValue("application/json", forHTTPHeaderField: "Content-Type") }
        if let t = address.token { req.setValue(t, forHTTPHeaderField: "X-Smooth-Token") }
        let (data, resp) = try await session.data(for: req)
        if let code = (resp as? HTTPURLResponse)?.statusCode, code != 200 { throw URLError(code == 401 ? .userAuthenticationRequired : .badServerResponse) }
        return try JSONDecoder().decode(HarnessList.self, from: data).harnesses
    }

    private func open() {
        guard let address, !closed else { return }
        store.connection = .connecting
        let t = session.webSocketTask(with: address.wsURL)
        task = t
        t.resume()
        // Identify ourselves before the first server frame; the engine ignores
        // it until the follow-up that reads it, by contract (unknown ⇒ ignored).
        t.send(.string(ClientFrame.hello(client: "smoothflow", version: Self.version).encodeText())) { _ in }
        receive(on: t)
    }

    private func receive(on t: URLSessionWebSocketTask) {
        t.receive { [weak self] result in
            Task { @MainActor in
                guard let self, self.task === t else { return }
                switch result {
                case let .failure(error):
                    self.dropped(error.localizedDescription)
                case let .success(message):
                    let data: Data
                    switch message {
                    case let .data(d): data = d
                    case let .string(s): data = Data(s.utf8)
                    @unknown default: data = Data()
                    }
                    if let frame = try? FlowFrame.decode(data) {
                        if case .hello = frame { self.backoff = 1 }
                        let effects = self.store.apply(frame)
                        if !effects.isEmpty { self.onEffects(effects) }
                    }
                    self.receive(on: t)
                }
            }
        }
    }

    private func dropped(_ reason: String) {
        task?.cancel()
        task = nil
        store.connection = .disconnected(reason: reason)
        guard !closed else { return }
        reconnectTimer?.invalidate()
        reconnectTimer = Timer.scheduledTimer(withTimeInterval: backoff, repeats: false) { [weak self] _ in
            Task { @MainActor in self?.open() }
        }
        backoff = min(backoff * 2, 15)
    }
}

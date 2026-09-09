import AppKit
import Combine
import UserNotifications

/// One `flow.close` the shell sent (th-883ce9).
struct CloseRequest: Equatable {
    var sessionId: String
    var closePearl: Bool
    var removeWorktree: Bool
    var force: Bool
}

/// The engine said no (dirty / unmerged worktree): what was asked, and why not.
struct CloseRefusal: Equatable {
    var request: CloseRequest
    var message: String
}

/// The coordinator: wires store ↔ client ↔ surfaces ↔ windows ↔ notifications.
/// Holds no session facts of its own — those live in `FlowStore`, fed by frames.
@MainActor
final class AppController: NSObject, ObservableObject, UNUserNotificationCenterDelegate {
    let store = FlowStore()
    let daemon = DaemonManager()
    let permissions = Permissions()
    private(set) lazy var client = FlowClient(store: store)

    @Published private(set) var handoffs: [String: Handoff] = [:]
    /// Every harness incl. hidden (Settings ▸ Harnesses); the pickers read
    /// `store.harnesses` (visible only) instead.
    @Published private(set) var allHarnesses: [HarnessInfo] = []
    @Published var thOutput: String?
    /// Settings ▸ Phones (th-d98fde): the paired phones, the QR on screen, and
    /// what the last scan produced.
    @Published private(set) var pairedPhones: PairingsList?
    @Published private(set) var pendingPairing: PairingBegin?
    @Published private(set) var pairingMessage: String?
    private var pairingPoll: Task<Void, Never>?
    /// th-883ce9: `flow.close` in flight, keyed by the `seq` the frame carried,
    /// and the engine's refusal per session (the card shows it with "Force close").
    private var pendingCloses: [Int: CloseRequest] = [:]
    @Published private(set) var closeRefusals: [String: CloseRefusal] = [:]
    private var nextSeq = 1
    var notifySettings = NotifySettings.load()

    private(set) var surfaces: [String: TerminalSurfaceView] = [:]
    private(set) var mainWindow: MainWindowController!
    private var inboxWindow: InboxWindowController?
    private var settingsWindow: SettingsWindowController?
    private var subscriptions: Set<AnyCancellable> = []

    static let onboardedKey = "onboarded"

    // MARK: lifecycle

    func start() {
        mainWindow = MainWindowController(app: self)
        mainWindow.showWindow(nil)
        mainWindow.window?.makeKeyAndOrderFront(nil)

        UNUserNotificationCenter.current().delegate = self
        AttentionNotifier.registerCategories()
        permissions.refresh()

        client.onEffects = { [weak self] effects in self?.handle(effects) }
        store.$focusedId.removeDuplicates().sink { [weak self] id in
            guard let self, let id else { return }
            self.mainWindow.center.showSession(id)
            self.mainWindow.center.refreshHeaders()
        }.store(in: &subscriptions)
        // @Published fires on willSet; hop once so the headers read the new value.
        store.$sessions.receive(on: DispatchQueue.main).sink { [weak self] live in
            self?.mainWindow.center.refreshHeaders()
            self?.pruneCloses(live)
        }.store(in: &subscriptions)
        daemon.onRestart = { [weak self] ep in self?.client.connect(to: ep) }

        connect()

        if !UserDefaults.standard.bool(forKey: Self.onboardedKey) {
            UserDefaults.standard.set(true, forKey: Self.onboardedKey)
            mainWindow.contentViewController?.presentSheet { dismiss in OnboardingView(permissions: self.permissions, daemon: self.daemon, dismiss: dismiss) }
        }
    }

    private func connect() {
        switch DaemonAddress.resolve(env: ProcessInfo.processInfo.environment, setting: UserDefaults.standard.string(forKey: DaemonAddress.defaultsKey)) {
        case var .external(ep):
            daemon.stop()
            let file = try? String(contentsOf: FileManager.default.homeDirectoryForCurrentUser.appendingPathComponent(".smooth/operator-token"), encoding: .utf8)
            ep.token = DaemonAddress.token(env: ProcessInfo.processInfo.environment, tokenFile: file)
            client.connect(to: ep)
        case .spawn:
            if UserDefaults.standard.object(forKey: "tmuxOwnedByApp") as? Bool ?? true { daemon.startTmuxServer() }
            if let ep = daemon.start() { client.connect(to: ep) } else { store.connection = .disconnected(reason: daemon.status) }
        }
    }

    func restartConnection() {
        client.disconnect()
        daemon.stop()
        connect()
    }

    /// Take the fleet down with the app: disconnect, stop the child daemon
    /// (bounded — see `DaemonManager.stop`), kill the app-owned tmux server.
    /// Idempotent: `applicationShouldTerminate` runs it, and
    /// `applicationWillTerminate` runs it again for the paths that skip the
    /// former (a SIGTERM to the app, a forced logout).
    private(set) var didShutdown = false
    func shutdown() {
        guard !didShutdown else { return }
        didShutdown = true
        client.disconnect()
        daemon.stop()
        if UserDefaults.standard.object(forKey: "tmuxOwnedByApp") as? Bool ?? true { daemon.stopTmuxServer() }
    }

    func activated() { permissions.refresh() }

    // MARK: effects

    private func handle(_ effects: [StoreEffect]) {
        for e in effects {
            switch e {
            case let .output(id, data):
                surfaces[id]?.feed(data)
            case let .attention(s):
                if let n = AttentionNotifier.notification(for: s, settings: notifySettings) {
                    AttentionNotifier.post(n, requestId: s.attention?.requestId, sound: notifySettings.sound)
                }
                NSApp.dockTile.badgeLabel = store.counts.needsYou > 0 ? String(store.counts.needsYou) : nil
            case let .finished(s):
                if let n = AttentionNotifier.notification(for: s, settings: notifySettings) {
                    AttentionNotifier.post(n, requestId: nil, sound: notifySettings.sound)
                }
                Task { await loadHandoff(for: s.id) }
            case .screen:
                break
            case .connected:
                reattachSurfaces()
            case let .relaunched(id):
                if let v = surfaces[id] { client.send(.attach(id: id, cols: v.gridSize.cols, rows: v.gridSize.rows)) }
            case let .handoff(id, h):
                handoffs[id] = h
            case let .error(ref, msg):
                if let ref, let req = pendingCloses.removeValue(forKey: ref) {
                    closeRefusals[req.sessionId] = CloseRefusal(request: req, message: msg)
                } else {
                    thOutput = msg
                }
            }
        }
        NSApp.dockTile.badgeLabel = store.counts.needsYou > 0 ? String(store.counts.needsYou) : nil
    }

    /// A close that succeeded ends in `flow.session.removed` (no effect of its
    /// own): forget the request, and any stale refusal, once the row is gone.
    private func pruneCloses(_ live: [String: Session]) {
        let pending = pendingCloses.filter { live[$0.value.sessionId] != nil }
        if pending.count != pendingCloses.count { pendingCloses = pending }
        let refusals = closeRefusals.filter { live[$0.key] != nil }
        if refusals.count != closeRefusals.count { closeRefusals = refusals }
    }

    // MARK: surfaces

    /// One surface per session, created on first focus and attached for the
    /// rest of its life (scrollback lives in the surface, not the engine).
    func surface(for id: String) -> TerminalSurfaceView {
        if let v = surfaces[id] { return v }
        let v = TerminalSurfaceView(sessionId: id)
        // A done/dead row keeps its surface for scrollback; input and the
        // layout-driven resize must not reach the engine (it answers
        // "not running" for each, which used to land in the rail).
        v.onInput = { [weak self] data in
            guard let self, self.store.sessions[id]?.isLive == true else { return }
            self.client.send(.input(id: id, data: data))
        }
        v.onResize = { [weak self] cols, rows in
            guard let self, self.store.sessions[id]?.isLive == true else { return }
            self.client.send(.resize(id: id, cols: cols, rows: rows))
        }
        v.onFocus = { [weak self] focused in if focused { self?.markRead(id) } }
        surfaces[id] = v
        if store.sessions[id]?.isLive == true {
            let g = v.gridSize
            client.send(.attach(id: id, cols: g.cols, rows: g.rows))
        }
        return v
    }

    /// After every (re)connect the engine has no attachments for us: re-attach
    /// every surface whose session still exists and drop the rest. Also runs
    /// after a daemon restart — the tmux sessions outlive it, so the surfaces
    /// get a fresh redraw rather than a blank pane.
    private func reattachSurfaces() {
        for (id, v) in surfaces {
            guard let s = store.sessions[id] else { surfaces[id] = nil; continue }
            guard s.isLive else { continue }
            let g = v.gridSize
            client.send(.attach(id: id, cols: g.cols, rows: g.rows))
        }
    }

    // MARK: actions (all flow frames)

    func focus(_ id: String) {
        guard store.sessions[id] != nil else { return }
        store.focusedId = id
        markRead(id)
        NSApp.activate(ignoringOtherApps: true)
        mainWindow.window?.makeKeyAndOrderFront(nil)
    }

    func focus(index: Int) {
        if let s = store.session(atIndex: index) { focus(s.id) }
    }

    func markRead(_ id: String) {
        guard store.sessions[id]?.unread == true else { AttentionNotifier.clear(sessionId: id); return }
        store.markReadLocally(id)
        client.send(.markRead(id: id))
        AttentionNotifier.clear(sessionId: id)
    }

    func steer(_ text: String, toAll: Bool) {
        let targets = toAll ? store.working.map(\.id) : [store.focusedId].compactMap { $0 }
        for id in targets { client.send(.send(id: id, text: text)) }
    }

    func approve(_ s: Session, _ decision: ApproveDecision) {
        guard let rid = s.attention?.requestId else { thOutput = "no request_id on \(s.id)"; return }
        client.send(.approve(id: s.id, requestId: rid, decision: decision))
        AttentionNotifier.clear(sessionId: s.id)
    }

    func kill(_ s: Session, resume: Bool) { client.send(.kill(id: s.id, resume: resume)) }

    /// th-883ce9: `flow.close` for a finished session. Tagged with a `seq` so
    /// the engine's refusal (`flow.error.ref`) lands on THIS card as a
    /// "Force close" offer instead of in the rail's generic output.
    func close(_ s: Session, closePearl: Bool, removeWorktree: Bool, force: Bool = false) {
        let seq = nextSeq
        nextSeq += 1
        let req = CloseRequest(sessionId: s.id, closePearl: closePearl, removeWorktree: removeWorktree, force: force)
        pendingCloses[seq] = req
        closeRefusals[s.id] = nil
        client.send(.close(id: s.id, closePearl: closePearl, removeWorktree: removeWorktree, force: force), seq: seq)
    }

    /// Resend the refused close with `force` — the user read the reason.
    func forceClose(_ s: Session) {
        guard let r = closeRefusals[s.id] else { return }
        close(s, closePearl: r.request.closePearl, removeWorktree: r.request.removeWorktree, force: true)
    }

    func dismissCloseRefusal(_ id: String) { closeRefusals[id] = nil }
    func newSession(_ n: NewSession) { client.send(.new(n)) }
    func fanoutNew(prompt: String, pearlId: String?, candidates: [FanOutCandidate]) {
        client.send(.fanoutNew(prompt: prompt, pearlId: pearlId, candidates: candidates))
    }
    func fanoutPick(fanOutId: String, winner: String) { client.send(.fanoutPick(fanOutId: fanOutId, winnerSessionId: winner)) }

    func loadAllHarnesses() async {
        if let h = try? await client.allHarnesses() { allHarnesses = h }
    }

    /// Persist a reorder / hide in the engine; the reply is the full list and
    /// the engine's `flow.harnesses` updates every picker.
    func setHarnessPrefs(order: [String]? = nil, hidden: [String]? = nil) async {
        do { allHarnesses = try await client.putHarnessPrefs(order: order, hidden: hidden) } catch { thOutput = "harness prefs: \(error.localizedDescription)" }
    }

    // ── phone pairing (th-d98fde) ────────────────────────────────────────────

    func loadPairings() async {
        do { pairedPhones = try await client.pairings() } catch { pairingMessage = "pairings: \(error.localizedDescription)" }
    }

    /// Mint a QR and poll until a phone scans it (or it expires). One at a time.
    func beginPairing() async {
        pairingPoll?.cancel()
        pairingMessage = nil
        do {
            let begin = try await client.beginPairing()
            pendingPairing = begin
            pairingPoll = Task { [weak self] in
                while !Task.isCancelled {
                    try? await Task.sleep(for: .seconds(1))
                    guard let self, let pending = self.pendingPairing, pending.pairingId == begin.pairingId else { return }
                    guard let poll = try? await self.client.pairingStatus(begin.pairingId) else { continue }
                    if poll.isPaired {
                        self.pairingMessage = "Paired \(poll.label ?? "phone") (\(poll.platform ?? "?"))"
                        self.pendingPairing = nil
                        await self.loadPairings()
                        return
                    }
                    if poll.isGone {
                        self.pairingMessage = "The link expired before it was scanned — show a new one."
                        self.pendingPairing = nil
                        return
                    }
                }
            }
        } catch {
            pairingMessage = "pair: \(error.localizedDescription)"
        }
    }

    func cancelPairing() {
        pairingPoll?.cancel()
        pendingPairing = nil
    }

    func revokePairing(_ device: String) async {
        do {
            _ = try await client.revokePairing(device)
            await loadPairings()
        } catch { pairingMessage = "revoke: \(error.localizedDescription)" }
    }

    func loadHandoff(for id: String) async {
        guard let h = try? await client.handoff(for: id) else { return }
        handoffs[id] = h
    }

    /// "Merge" on a finished session opens its PR — merging is a PR-review act,
    /// not something the shell does blind.
    func merge(_ s: Session) {
        if let u = handoffs[s.id]?.pr?.url, let url = URL(string: u) { NSWorkspace.shared.open(url) }
    }

    /// Pearl-rail buttons shell `th` in the worktree; output goes under the rail.
    func runTh(_ args: [String], in worktree: String) {
        guard let th = Shell.which("th") else { thOutput = "th not found"; return }
        thOutput = "$ th \(args.joined(separator: " "))…"
        Task.detached { [weak self] in
            let out = Shell.run(th, args, cwd: worktree)
            await MainActor.run { self?.thOutput = out.trimmingCharacters(in: .whitespacesAndNewlines) }
        }
    }

    // MARK: windows

    func toggleInbox() {
        if inboxWindow == nil { inboxWindow = InboxWindowController(app: self) }
        guard let w = inboxWindow?.window else { return }
        if w.isVisible { w.orderOut(nil) } else { w.makeKeyAndOrderFront(nil) }
    }

    func showSettings() {
        if settingsWindow == nil { settingsWindow = SettingsWindowController(app: self) }
        settingsWindow?.window?.makeKeyAndOrderFront(nil)
    }

    func showNewSession() {
        mainWindow.contentViewController?.presentSheet { dismiss in NewSessionSheet(app: self, dismiss: dismiss) }
    }

    func showFanOut(existing: String? = nil) {
        mainWindow.contentViewController?.presentSheet { dismiss in FanOutSheet(store: self.store, app: self, existingId: existing, dismiss: dismiss) }
    }

    func showTab(_ t: CenterTab) { mainWindow.center.tab = t }

    // MARK: notifications → focus / approve

    nonisolated func userNotificationCenter(_ center: UNUserNotificationCenter, didReceive response: UNNotificationResponse) async {
        let info = response.notification.request.content.userInfo
        let sessionId = info[AttentionNotifier.sessionKey] as? String ?? ""
        let action = response.actionIdentifier
        await MainActor.run {
            guard let s = store.sessions[sessionId] else { return }
            switch action {
            case "allow": approve(s, .allow)
            case "deny": approve(s, .deny)
            default: focus(sessionId)
            }
        }
    }

    nonisolated func userNotificationCenter(_ center: UNUserNotificationCenter, willPresent notification: UNNotification) async -> UNNotificationPresentationOptions {
        [.banner, .sound, .list]
    }
}

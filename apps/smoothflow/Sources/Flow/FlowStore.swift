import Combine
import Foundation

/// What the reducer asks the outside world to do after applying a frame.
/// Kept out of the store so `apply` stays a pure, testable function.
enum StoreEffect: Equatable {
    /// Raw PTY bytes for an attached session → the terminal surface.
    case output(id: String, data: Data)
    /// A session newly needs a human (permission / question / limit / held / crashed).
    case attention(Session)
    /// A session just reached `done` or `dead`.
    case finished(Session)
    /// Snapshot text arrived (phones use this; desktop shows it while unattached).
    case screen(id: String, text: String)
    /// A (re)connect completed: `flow.hello` arrived. Surfaces must re-attach.
    case connected
    /// The engine (re)launched this session's process (new pid): its old PTY
    /// attachment is gone, so an existing surface must re-attach.
    case relaunched(id: String)
    /// The engine pushed a handoff packet (`flow.handoff`).
    case handoff(id: String, Handoff)
    case error(String)
}

enum ConnectionState: Equatable {
    case disconnected(reason: String?)
    case connecting
    case connected(DaemonInfo)

    var isConnected: Bool { if case .connected = self { return true } else { return false } }
}

/// The shell's only view-model: a projection of `flow.*` frames. It never
/// invents a fact — every field here was sent by the engine.
@MainActor
final class FlowStore: ObservableObject {
    @Published private(set) var sessions: [String: Session] = [:]
    /// Insertion order — ⌘1…9 and the sidebar order are stable across updates.
    @Published private(set) var order: [String] = []
    @Published var focusedId: String?
    @Published var connection: ConnectionState = .disconnected(reason: nil)
    @Published private(set) var fanOuts: [String: FanOut] = [:]
    @Published private(set) var fanOutCandidates: [String: [String]] = [:]
    @Published var lastError: String?
    /// Activity per session from `flow.event`, newest last. ponytail: capped
    /// at `eventCap` per session; page from the engine if history matters.
    @Published private(set) var events: [String: [FlowEvent]] = [:]
    static let eventCap = 500
    /// The harnesses a picker offers (th-0f6126): the engine's order, hidden
    /// ones already dropped. From `flow.hello`, replaced by `flow.harnesses`.
    @Published private(set) var harnesses: [HarnessInfo] = []

    var ordered: [Session] { order.compactMap { sessions[$0] } }
    var focused: Session? { focusedId.flatMap { sessions[$0] } }

    /// Sidebar groups, by project (in first-seen order), plain shells last.
    var grouped: [(project: String, sessions: [Session])] {
        var groups: [(String, [Session])] = []
        for s in ordered {
            let key = s.kind == "shell" ? "shells" : s.projectName
            if let i = groups.firstIndex(where: { $0.0 == key }) { groups[i].1.append(s) } else { groups.append((key, [s])) }
        }
        return groups.map { (project: $0.0, sessions: $0.1) }
    }

    var counts: (working: Int, needsYou: Int, done: Int, idle: Int) {
        let all = ordered
        return (all.filter { $0.state == .working || $0.state == .starting }.count,
                all.filter(\.needsYou).count,
                all.filter { $0.state == .done }.count,
                all.filter { $0.state == .idle }.count)
    }

    var needsYou: [Session] { ordered.filter(\.needsYou) }
    var finished: [Session] { ordered.filter { $0.state == .done || $0.state == .dead } }
    var working: [Session] { ordered.filter { $0.state == .working || $0.state == .starting } }
    var unreadCount: Int { ordered.filter(\.unread).count }

    func session(atIndex i: Int) -> Session? { i < order.count ? sessions[order[i]] : nil }

    func fanOut(for session: Session) -> (FanOut, [Session])? {
        guard let fid = session.fanOutId, let f = fanOuts[fid] else { return nil }
        return (f, (fanOutCandidates[fid] ?? []).compactMap { sessions[$0] })
    }

    @discardableResult
    func apply(_ frame: FlowFrame) -> [StoreEffect] {
        switch frame {
        case let .hello(daemon, list, harnessList):
            connection = .connected(daemon)
            harnesses = HarnessOrdering.visible(harnessList)
            sessions = Dictionary(uniqueKeysWithValues: list.map { ($0.id, $0) })
            order = list.map(\.id)
            if let f = focusedId, sessions[f] == nil { focusedId = nil }
            // Land on something alive: a dead row at the top of the fleet was
            // what the first real-engine launch focused (and tried to attach).
            if focusedId == nil { focusedId = ordered.first(where: \.isLive)?.id ?? order.first }
            events = events.filter { sessions[$0.key] != nil }
            return [.connected]

        case let .session(s):
            let before = sessions[s.id]
            upsert(s)
            var effects: [StoreEffect] = []
            if let b = before, let pid = s.pid, b.pid != pid, s.state != .done, s.state != .dead { effects.append(.relaunched(id: s.id)) }
            if s.needsYou, before?.needsYou != true || before?.attention != s.attention { effects.append(.attention(s)) }
            if (s.state == .done || s.state == .dead), before?.state != s.state { effects.append(.finished(s)) }
            return effects

        case let .sessionRemoved(id):
            sessions[id] = nil
            events[id] = nil
            order.removeAll { $0 == id }
            for (fid, ids) in fanOutCandidates { fanOutCandidates[fid] = ids.filter { $0 != id } }
            if focusedId == id { focusedId = order.first }
            return []

        case let .output(id, _, data):
            guard sessions[id] != nil, !data.isEmpty else { return [] }
            return [.output(id: id, data: data)]

        case let .screen(id, _, _, text):
            return sessions[id] != nil ? [.screen(id: id, text: text)] : []

        case let .attention(id, attention):
            guard var s = sessions[id] else { return [] }
            // `flow.session` carries the same change; this frame exists so
            // notifications can key on it without diffing. Idempotent.
            let changed = s.attention != attention
            s.attention = attention
            sessions[id] = s
            return changed && attention != nil ? [.attention(s)] : []

        case let .fanout(f, candidates):
            fanOuts[f.id] = f
            for c in candidates { upsert(c) }
            fanOutCandidates[f.id] = candidates.map(\.id)
            return []

        case let .event(e):
            guard sessions[e.sessionId] != nil else { return [] }
            var list = events[e.sessionId] ?? []
            guard !list.contains(where: { $0.eventId == e.eventId }) else { return [] }
            list.append(e)
            if list.count > Self.eventCap { list.removeFirst(list.count - Self.eventCap) }
            events[e.sessionId] = list
            return []

        case let .handoff(id, h):
            return sessions[id] != nil ? [.handoff(id: id, h)] : []

        case let .harnesses(list):
            harnesses = HarnessOrdering.visible(list)
            return []

        case let .error(_, code, message):
            lastError = "\(code): \(message)"
            return [.error(lastError!)]

        case .unknown:
            return []
        }
    }

    private func upsert(_ s: Session) {
        if sessions[s.id] == nil { order.append(s.id) }
        sessions[s.id] = s
        if focusedId == nil { focusedId = s.id }
    }

    /// The human name of a session kind — the harness's display name when the
    /// engine listed it, else the raw kind (`shell`, a hidden harness).
    func displayName(forKind kind: String) -> String {
        if kind == "shell" { return "Shell" }
        return harnesses.first { $0.name == kind }?.displayName ?? kind
    }

    /// Local-only echo so the badge clears immediately; the engine confirms via `flow.session`.
    func markReadLocally(_ id: String) {
        sessions[id]?.unread = false
    }
}

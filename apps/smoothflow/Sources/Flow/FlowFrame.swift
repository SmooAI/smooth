import Foundation

// The v0 flow protocol (smoothflow-protocol.md). Every frame is one JSON
// object `{"channel":"flow","type":"<name>", ...}`. Unknown types decode to
// `.unknown` and are ignored, never fatal. The shell holds no state of its own:
// everything it shows comes from these frames.

enum SessionState: String, Codable, Equatable {
    case starting, working, idle, needsYou = "needs_you", limited, done, dead, unknown

    init(from decoder: Decoder) throws {
        let raw = try decoder.singleValueContainer().decode(String.self)
        self = SessionState(rawValue: raw) ?? .unknown
    }
}

enum AttentionReason: String, Codable, Equatable {
    case permission, question, usageLimit = "usage_limit", crashed, held, unknown

    init(from decoder: Decoder) throws {
        let raw = try decoder.singleValueContainer().decode(String.self)
        self = AttentionReason(rawValue: raw) ?? .unknown
    }
}

struct Attention: Codable, Equatable {
    var reason: AttentionReason
    var detail: String?
    var resumeAt: String?
    var requestId: String?
    /// Holder pid for `held` (engine sends it in `detail` or as `pid`).
    var pid: Int?

    enum CodingKeys: String, CodingKey { case reason, detail, resumeAt = "resume_at", requestId = "request_id", pid }

    init(reason: AttentionReason, detail: String? = nil, resumeAt: String? = nil, requestId: String? = nil, pid: Int? = nil) {
        self.reason = reason
        self.detail = detail
        self.resumeAt = resumeAt
        self.requestId = requestId
        self.pid = pid
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        reason = try c.decodeIfPresent(AttentionReason.self, forKey: .reason) ?? .unknown
        detail = try c.decodeFlexibleString(forKey: .detail)
        resumeAt = try c.decodeIfPresent(String.self, forKey: .resumeAt)
        requestId = try c.decodeFlexibleString(forKey: .requestId)
        pid = try c.decodeIfPresent(Int.self, forKey: .pid)
    }

    var resumeDate: Date? { resumeAt.flatMap { ISO8601DateFormatter.flexible.date(from: $0) } }
}

struct Session: Codable, Equatable, Identifiable {
    var id: String
    var kind: String
    var title: String
    var project: String
    var worktree: String
    var branch: String?
    var pearlId: String?
    var agentSessionId: String?
    var argv: [String]
    var tmuxSession: String?
    var pid: Int?
    var state: SessionState
    var attention: Attention?
    var fanOutId: String?
    var createdAt: String
    var updatedAt: String
    var endedAt: String?
    var exitCode: Int?
    var unread: Bool

    enum CodingKeys: String, CodingKey {
        case id, kind, title, project, worktree, branch, argv, pid, state, attention, unread
        case pearlId = "pearl_id", agentSessionId = "agent_session_id", tmuxSession = "tmux_session"
        case fanOutId = "fan_out_id", createdAt = "created_at", updatedAt = "updated_at", endedAt = "ended_at", exitCode = "exit_code"
    }

    init(id: String, kind: String = "claude", title: String = "", project: String = "", worktree: String = "", branch: String? = nil,
         pearlId: String? = nil, agentSessionId: String? = nil, argv: [String] = [], tmuxSession: String? = nil, pid: Int? = nil,
         state: SessionState = .idle, attention: Attention? = nil, fanOutId: String? = nil, createdAt: String = "", updatedAt: String = "",
         endedAt: String? = nil, exitCode: Int? = nil, unread: Bool = false) {
        self.id = id; self.kind = kind; self.title = title; self.project = project; self.worktree = worktree; self.branch = branch
        self.pearlId = pearlId; self.agentSessionId = agentSessionId; self.argv = argv; self.tmuxSession = tmuxSession; self.pid = pid
        self.state = state; self.attention = attention; self.fanOutId = fanOutId; self.createdAt = createdAt; self.updatedAt = updatedAt
        self.endedAt = endedAt; self.exitCode = exitCode; self.unread = unread
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(String.self, forKey: .id)
        kind = try c.decodeIfPresent(String.self, forKey: .kind) ?? "shell"
        title = try c.decodeIfPresent(String.self, forKey: .title) ?? ""
        project = try c.decodeIfPresent(String.self, forKey: .project) ?? ""
        worktree = try c.decodeIfPresent(String.self, forKey: .worktree) ?? ""
        branch = try c.decodeIfPresent(String.self, forKey: .branch)
        pearlId = try c.decodeIfPresent(String.self, forKey: .pearlId)
        agentSessionId = try c.decodeIfPresent(String.self, forKey: .agentSessionId)
        // The row stores argv as JSON text; the engine may forward it either way.
        if let arr = try? c.decodeIfPresent([String].self, forKey: .argv) {
            argv = arr
        } else if let text = try? c.decodeIfPresent(String.self, forKey: .argv),
                  let data = text.data(using: .utf8), let arr = try? JSONDecoder().decode([String].self, from: data) {
            argv = arr
        } else {
            argv = []
        }
        tmuxSession = try c.decodeIfPresent(String.self, forKey: .tmuxSession)
        pid = try c.decodeIfPresent(Int.self, forKey: .pid)
        state = try c.decodeIfPresent(SessionState.self, forKey: .state) ?? .unknown
        attention = try c.decodeIfPresent(Attention.self, forKey: .attention)
        fanOutId = try c.decodeIfPresent(String.self, forKey: .fanOutId)
        createdAt = try c.decodeIfPresent(String.self, forKey: .createdAt) ?? ""
        updatedAt = try c.decodeIfPresent(String.self, forKey: .updatedAt) ?? ""
        endedAt = try c.decodeIfPresent(String.self, forKey: .endedAt)
        exitCode = try c.decodeIfPresent(Int.self, forKey: .exitCode)
        unread = try c.decodeIfPresent(Bool.self, forKey: .unread) ?? false
    }

    /// Sidebar label: pearl id when there is one, else the title.
    var label: String { pearlId.map { "\($0) \(title)" } ?? title }
    var projectName: String { (project as NSString).lastPathComponent.isEmpty ? project : (project as NSString).lastPathComponent }
    var needsYou: Bool { state == .needsYou || state == .limited || attention?.reason == .held }
    /// Has (or will have) a PTY to attach to. `done`/`dead` rows keep their
    /// surface scrollback but must not be attached — the engine refuses.
    var isLive: Bool { state != .done && state != .dead }
}

struct FanOut: Codable, Equatable, Identifiable {
    var id: String
    var prompt: String
    var baseCommit: String?
    var pearlId: String?
    var createdAt: String?
    var winnerSessionId: String?

    enum CodingKeys: String, CodingKey {
        case id, prompt, baseCommit = "base_commit", pearlId = "pearl_id", createdAt = "created_at", winnerSessionId = "winner_session_id"
    }
}

/// One harness the engine can launch (th-0f6126) — a row of
/// `flow.hello.harnesses` / `flow.harnesses` / `GET /api/flow/harnesses`.
/// Pickers never invent a kind: they render exactly this list.
struct HarnessInfo: Codable, Equatable, Identifiable {
    var name: String
    var displayName: String
    var kind: String
    var installed: Bool
    var binaryPath: String?
    /// `hooks` | `scrape` | `native`
    var stateSource: String
    var hidden: Bool
    var orderIndex: Int
    /// Why `installed` is false.
    var reason: String?
    var origin: String
    var id: String { name }

    enum CodingKeys: String, CodingKey {
        case name, kind, installed, hidden, reason, origin
        case displayName = "display_name", binaryPath = "binary_path", stateSource = "state_source", orderIndex = "order_index"
    }

    init(name: String, displayName: String? = nil, kind: String? = nil, installed: Bool = true, binaryPath: String? = nil,
         stateSource: String = "hooks", hidden: Bool = false, orderIndex: Int = 0, reason: String? = nil, origin: String = "builtin") {
        self.name = name; self.displayName = displayName ?? name; self.kind = kind ?? name; self.installed = installed
        self.binaryPath = binaryPath; self.stateSource = stateSource; self.hidden = hidden; self.orderIndex = orderIndex
        self.reason = reason; self.origin = origin
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        name = try c.decode(String.self, forKey: .name)
        displayName = try c.decodeIfPresent(String.self, forKey: .displayName) ?? name
        kind = try c.decodeIfPresent(String.self, forKey: .kind) ?? name
        installed = try c.decodeIfPresent(Bool.self, forKey: .installed) ?? false
        binaryPath = try c.decodeIfPresent(String.self, forKey: .binaryPath)
        stateSource = try c.decodeIfPresent(String.self, forKey: .stateSource) ?? "hooks"
        hidden = try c.decodeIfPresent(Bool.self, forKey: .hidden) ?? false
        orderIndex = try c.decodeIfPresent(Int.self, forKey: .orderIndex) ?? 0
        reason = try c.decodeIfPresent(String.self, forKey: .reason)
        origin = try c.decodeIfPresent(String.self, forKey: .origin) ?? ""
    }

    /// The picker label: the display name, and why it is greyed out when it is.
    var pickerLabel: String { installed ? displayName : "\(displayName) — \(reason ?? "not installed")" }
}

/// Pure helpers behind the Settings ▸ Harnesses pane (XCTested without UI).
enum HarnessOrdering {
    /// `names` with the element at `index` moved one step (`-1` up / `+1` down);
    /// a move off either end is a no-op.
    static func moved(_ names: [String], at index: Int, by delta: Int) -> [String] {
        let target = index + delta
        guard names.indices.contains(index), names.indices.contains(target) else { return names }
        var out = names
        out.swapAt(index, target)
        return out
    }

    /// The `hidden` list with `name` toggled.
    static func toggled(_ hidden: [String], _ name: String) -> [String] {
        hidden.contains(name) ? hidden.filter { $0 != name } : hidden + [name]
    }

    /// Picker candidates: `all` in order, hidden ones dropped.
    static func visible(_ all: [HarnessInfo]) -> [HarnessInfo] { all.filter { !$0.hidden } }
}

struct DaemonInfo: Codable, Equatable {
    var version: String
    var machineLabel: String
    enum CodingKeys: String, CodingKey { case version, machineLabel = "machine_label" }

    init(version: String, machineLabel: String) { self.version = version; self.machineLabel = machineLabel }
    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        version = try c.decodeIfPresent(String.self, forKey: .version) ?? "?"
        machineLabel = try c.decodeIfPresent(String.self, forKey: .machineLabel) ?? ""
    }
}

/// `GET /api/flow/sessions/{id}/handoff` — the pearl rail's data.
struct Handoff: Codable, Equatable {
    struct Pearl: Codable, Equatable {
        var id: String?
        var title: String?
        var status: String?
        var priority: Int?
        var labels: [String]?
    }
    struct Packet: Codable, Equatable {
        var worktree: String?
        var branch: String?
        var head: String?
        var dirty: [String]?
        var agentSessionId: String?
        var next: String?
        enum CodingKeys: String, CodingKey { case worktree, branch, head, dirty, next, agentSessionId = "agent_session_id" }
    }
    struct Checkpoint: Codable, Equatable, Identifiable {
        var at: String
        var note: String
        var auto: Bool?
        var id: String { at + note }
    }
    struct PR: Codable, Equatable {
        var number: Int?
        var url: String?
        var ci: String?
    }
    var pearl: Pearl?
    var handoff: Packet?
    var checkpoints: [Checkpoint]?
    var blocks: [String]?
    var pr: PR?
}

/// `flow.event {id, event_id, at, kind, text}` — one line of a session's
/// activity (tool calls, hook events, supervision) as the engine saw it.
struct FlowEvent: Codable, Equatable, Identifiable {
    var sessionId: String
    var eventId: String
    var at: String
    var kind: String
    var text: String
    var id: String { eventId }

    enum CodingKeys: String, CodingKey { case sessionId = "id", eventId = "event_id", at, kind, text }

    init(sessionId: String, eventId: String, at: String, kind: String, text: String) {
        self.sessionId = sessionId; self.eventId = eventId; self.at = at; self.kind = kind; self.text = text
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        sessionId = try c.decode(String.self, forKey: .sessionId)
        at = try c.decodeIfPresent(String.self, forKey: .at) ?? ""
        kind = try c.decodeIfPresent(String.self, forKey: .kind) ?? ""
        text = try c.decodeFlexibleString(forKey: .text) ?? ""
        eventId = try c.decodeFlexibleString(forKey: .eventId) ?? "\(sessionId)/\(at)/\(kind)/\(text.hashValue)"
    }

    /// Events that mean "Big Smooth needs you" — the only ones that earn amber.
    var needsYou: Bool { ["permission", "question", "usage_limit", "held", "crashed", "needs_you"].contains(kind) }
}

/// Engine → client.
enum FlowFrame: Equatable {
    case hello(daemon: DaemonInfo, sessions: [Session], harnesses: [HarnessInfo] = [])
    /// Additive (th-0f6126): the visible harness list changed.
    case harnesses([HarnessInfo])
    case session(Session)
    case sessionRemoved(id: String)
    case output(id: String, seq: UInt64, data: Data)
    case screen(id: String, cols: Int, rows: Int, text: String)
    case attention(id: String, attention: Attention?)
    case fanout(FanOut, candidates: [Session])
    case error(ref: Int?, code: String, message: String)
    /// Additive (engine follow-up): one activity line for a session.
    case event(FlowEvent)
    /// Additive: the handoff packet pushed instead of polled — the same shape
    /// as `GET /api/flow/sessions/{id}/handoff`, plus `id`.
    case handoff(id: String, Handoff)
    case unknown(type: String)

    private struct Key: CodingKey {
        var stringValue: String
        var intValue: Int? { nil }
        init(_ s: String) { stringValue = s }
        init?(stringValue: String) { self.stringValue = stringValue }
        init?(intValue: Int) { nil }
    }

    static func decode(_ data: Data) throws -> FlowFrame {
        try JSONDecoder().decode(Box.self, from: data).frame
    }

    private struct Box: Decodable {
        let frame: FlowFrame
        init(from decoder: Decoder) throws {
            let c = try decoder.container(keyedBy: Key.self)
            let type = try c.decodeIfPresent(String.self, forKey: Key("type")) ?? ""
            switch type {
            case "flow.hello":
                frame = .hello(daemon: try c.decodeIfPresent(DaemonInfo.self, forKey: Key("daemon")) ?? DaemonInfo(version: "?", machineLabel: ""),
                               sessions: try c.decodeIfPresent([Session].self, forKey: Key("sessions")) ?? [],
                               harnesses: (try? c.decodeIfPresent([HarnessInfo].self, forKey: Key("harnesses"))) ?? [])
            case "flow.harnesses":
                frame = .harnesses((try? c.decodeIfPresent([HarnessInfo].self, forKey: Key("harnesses"))) ?? [])
            case "flow.session":
                frame = .session(try c.decode(Session.self, forKey: Key("session")))
            case "flow.session.removed":
                frame = .sessionRemoved(id: try c.decode(String.self, forKey: Key("id")))
            case "flow.output":
                let b64 = try c.decodeIfPresent(String.self, forKey: Key("data_b64")) ?? ""
                frame = .output(id: try c.decode(String.self, forKey: Key("id")),
                                seq: try c.decodeIfPresent(UInt64.self, forKey: Key("seq")) ?? 0,
                                data: Data(base64Encoded: b64) ?? Data())
            case "flow.screen":
                frame = .screen(id: try c.decode(String.self, forKey: Key("id")),
                                cols: try c.decodeIfPresent(Int.self, forKey: Key("cols")) ?? 0,
                                rows: try c.decodeIfPresent(Int.self, forKey: Key("rows")) ?? 0,
                                text: try c.decodeIfPresent(String.self, forKey: Key("text")) ?? "")
            case "flow.attention":
                frame = .attention(id: try c.decode(String.self, forKey: Key("id")),
                                   attention: try c.decodeIfPresent(Attention.self, forKey: Key("attention")))
            case "flow.fanout":
                frame = .fanout(try c.decode(FanOut.self, forKey: Key("fan_out")),
                                candidates: try c.decodeIfPresent([Session].self, forKey: Key("candidates")) ?? [])
            case "flow.event":
                frame = .event(try FlowEvent(from: decoder))
            case "flow.handoff":
                frame = .handoff(id: try c.decode(String.self, forKey: Key("id")), try Handoff(from: decoder))
            case "flow.error":
                frame = .error(ref: try? c.decodeIfPresent(Int.self, forKey: Key("ref")),
                               code: try c.decodeFlexibleString(forKey: Key("code")) ?? "unknown",
                               message: try c.decodeIfPresent(String.self, forKey: Key("message")) ?? "")
            default:
                frame = .unknown(type: type)
            }
        }
    }
}

/// Client → engine. `encode()` yields the wire JSON.
enum ClientFrame: Equatable {
    /// Sent once per connection so the engine knows who is looking.
    case hello(client: String, version: String)
    case attach(id: String, cols: Int, rows: Int)
    case detach(id: String)
    case input(id: String, data: Data)
    case resize(id: String, cols: Int, rows: Int)
    case snapshot(id: String)
    case new(NewSession)
    case send(id: String, text: String)
    case approve(id: String, requestId: String, decision: ApproveDecision)
    case kill(id: String, resume: Bool)
    case fanoutNew(prompt: String, pearlId: String?, candidates: [FanOutCandidate])
    case fanoutPick(fanOutId: String, winnerSessionId: String)
    case markRead(id: String)
    /// th-e126cc / th-883ce9: finish a session for good — close its pearl,
    /// remove the merged worktree + branch, drop the row. The engine refuses a
    /// dirty or unmerged worktree (`flow.error`, nothing touched) unless `force`.
    case close(id: String, closePearl: Bool, removeWorktree: Bool, force: Bool)

    var type: String {
        switch self {
        case .hello: "flow.hello"
        case .attach: "flow.attach"
        case .detach: "flow.detach"
        case .input: "flow.input"
        case .resize: "flow.resize"
        case .snapshot: "flow.snapshot"
        case .new: "flow.new"
        case .send: "flow.send"
        case .approve: "flow.approve"
        case .kill: "flow.kill"
        case .fanoutNew: "flow.fanout.new"
        case .fanoutPick: "flow.fanout.pick"
        case .markRead: "flow.mark_read"
        case .close: "flow.close"
        }
    }

    var fields: [String: Any] {
        switch self {
        case let .hello(client, version): ["client": client, "version": version]
        case let .attach(id, cols, rows): ["id": id, "cols": cols, "rows": rows]
        case let .detach(id): ["id": id]
        case let .input(id, data): ["id": id, "data_b64": data.base64EncodedString()]
        case let .resize(id, cols, rows): ["id": id, "cols": cols, "rows": rows]
        case let .snapshot(id): ["id": id]
        case let .new(n): n.fields
        case let .send(id, text): ["id": id, "text": text]
        case let .approve(id, requestId, decision): ["id": id, "request_id": requestId, "decision": decision.rawValue]
        case let .kill(id, resume): ["id": id, "resume": resume]
        case let .fanoutNew(prompt, pearlId, candidates):
            ["prompt": prompt, "pearl_id": pearlId as Any, "candidates": candidates.map(\.fields)]
        case let .fanoutPick(fanOutId, winner): ["fan_out_id": fanOutId, "winner_session_id": winner]
        case let .markRead(id): ["id": id]
        case let .close(id, closePearl, removeWorktree, force):
            ["id": id, "close_pearl": closePearl, "remove_worktree": removeWorktree, "force": force]
        }
    }

    /// `seq` is a client-chosen correlation id: the engine echoes it back as
    /// `flow.error.ref`, which is how a refused `flow.close` finds its card.
    /// Omitted from the wire when nil, so every frame without one is unchanged.
    func encode(seq: Int? = nil) -> Data {
        var obj = fields
        obj["channel"] = "flow"
        obj["type"] = type
        if let seq { obj["seq"] = seq }
        // `Any?` nils become NSNull so optional keys serialize as JSON null.
        let cleaned = obj.mapValues { v -> Any in if case Optional<Any>.none = v { return NSNull() } else { return v } }
        return (try? JSONSerialization.data(withJSONObject: cleaned)) ?? Data()
    }

    /// The wire text. The engine reads TEXT WebSocket messages only (a binary
    /// frame is silently skipped), so this is what actually goes on the socket.
    func encodeText(seq: Int? = nil) -> String { String(decoding: encode(seq: seq), as: UTF8.self) }
}

enum ApproveDecision: String { case allow, deny, allowSession = "allow_session" }

/// `GET /api/flow/infer` — what a session started in a directory would be
/// working on (th-c103c1). Everything but `title` may be absent.
struct InferredContext: Codable, Equatable {
    var cwd: String = ""
    var worktree: String = ""
    var project: String = ""
    var isGit: Bool = false
    var branch: String?
    var pearlId: String?
    var pearlTitle: String?
    /// `store` | `branch` | `worktree` — where the pearl id came from.
    var pearlSource: String?
    var jiraKey: String?
    var title: String = ""

    enum CodingKeys: String, CodingKey {
        case cwd, worktree, project, branch, title
        case isGit = "is_git", pearlId = "pearl_id", pearlTitle = "pearl_title", pearlSource = "pearl_source", jiraKey = "jira_key"
    }

    /// The one-line context the dialog shows under the title: pearl · Jira ·
    /// branch · worktree, skipping whatever is not known.
    var summary: String {
        [pearlId, jiraKey, branch, worktree].compactMap { $0 }.filter { !$0.isEmpty }.joined(separator: " · ")
    }
}

struct NewSession: Equatable {
    var kind: String = "claude"
    var worktree: String?
    var project: String?
    var pearlId: String?
    var prompt: String?
    var argv: [String]?
    var title: String?

    var fields: [String: Any] {
        ["kind": kind, "worktree": worktree as Any, "project": project as Any, "pearl_id": pearlId as Any,
         "prompt": prompt as Any, "argv": argv as Any, "title": title as Any]
    }
}

struct FanOutCandidate: Equatable {
    var kind: String
    var model: String?
    var label: String
    var fields: [String: Any] { ["kind": kind, "model": model as Any, "label": label] }
}

extension KeyedDecodingContainer {
    /// Strings that the engine may send as a string, number, or object.
    func decodeFlexibleString(forKey key: Key) throws -> String? {
        if let s = try? decodeIfPresent(String.self, forKey: key) { return s }
        if let i = try? decodeIfPresent(Int.self, forKey: key) { return String(i) }
        if let d = try? decodeIfPresent([String: AnyJSON].self, forKey: key) { return AnyJSON.render(d) }
        return nil
    }
}

/// Just enough JSON-any to stringify nested `detail` objects.
enum AnyJSON: Decodable {
    case string(String), number(Double), bool(Bool), null, array([AnyJSON]), object([String: AnyJSON])

    init(from decoder: Decoder) throws {
        let c = try decoder.singleValueContainer()
        if c.decodeNil() { self = .null }
        else if let b = try? c.decode(Bool.self) { self = .bool(b) }
        else if let n = try? c.decode(Double.self) { self = .number(n) }
        else if let s = try? c.decode(String.self) { self = .string(s) }
        else if let a = try? c.decode([AnyJSON].self) { self = .array(a) }
        else { self = .object(try c.decode([String: AnyJSON].self)) }
    }

    var text: String {
        switch self {
        case let .string(s): s
        case let .number(n): n == n.rounded() ? String(Int(n)) : String(n)
        case let .bool(b): String(b)
        case .null: "null"
        case let .array(a): a.map(\.text).joined(separator: ", ")
        case let .object(o): AnyJSON.render(o)
        }
    }

    static func render(_ o: [String: AnyJSON]) -> String {
        // Common engine shapes: {"command": "..."} / {"tool":..., "input":...}
        if let cmd = o["command"] { return cmd.text }
        return o.keys.sorted().map { "\($0): \(o[$0]!.text)" }.joined(separator: " · ")
    }
}

extension ISO8601DateFormatter {
    /// RFC3339 with or without fractional seconds.
    static let flexible: FlexibleISO8601 = FlexibleISO8601()
}

struct FlexibleISO8601 {
    private let plain: ISO8601DateFormatter = { let f = ISO8601DateFormatter(); f.formatOptions = [.withInternetDateTime]; return f }()
    private let fractional: ISO8601DateFormatter = {
        let f = ISO8601DateFormatter(); f.formatOptions = [.withInternetDateTime, .withFractionalSeconds]; return f
    }()
    func date(from s: String) -> Date? { fractional.date(from: s) ?? plain.date(from: s) }
}

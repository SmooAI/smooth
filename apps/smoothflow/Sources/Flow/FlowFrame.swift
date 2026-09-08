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

/// Engine → client.
enum FlowFrame: Equatable {
    case hello(daemon: DaemonInfo, sessions: [Session])
    case session(Session)
    case sessionRemoved(id: String)
    case output(id: String, seq: UInt64, data: Data)
    case screen(id: String, cols: Int, rows: Int, text: String)
    case attention(id: String, attention: Attention?)
    case fanout(FanOut, candidates: [Session])
    case error(ref: Int?, code: String, message: String)
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
                               sessions: try c.decodeIfPresent([Session].self, forKey: Key("sessions")) ?? [])
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
            case "flow.error":
                frame = .error(ref: try c.decodeIfPresent(Int.self, forKey: Key("ref")),
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

    var type: String {
        switch self {
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
        }
    }

    var fields: [String: Any] {
        switch self {
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
        }
    }

    func encode() -> Data {
        var obj = fields
        obj["channel"] = "flow"
        obj["type"] = type
        // `Any?` nils become NSNull so optional keys serialize as JSON null.
        let cleaned = obj.mapValues { v -> Any in if case Optional<Any>.none = v { return NSNull() } else { return v } }
        return (try? JSONSerialization.data(withJSONObject: cleaned)) ?? Data()
    }
}

enum ApproveDecision: String { case allow, deny, allowSession = "allow_session" }

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

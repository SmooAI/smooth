import Foundation

/// Terminal stream ordering around `flow.replay` (Client Spec §10, th-c61966):
/// the Swift twin of `smooth_flow_client::replay::ReplayOrder`, held to the
/// same `spec/vectors/replay.json` by `ConformanceVectorTests`.
///
/// An engine that owns its PTYs answers `flow.attach{replay:true}` with a VT
/// snapshot current through some `seq`; live `flow.output` follows. Feed this
/// the session's frames in arrival order and do what it returns:
/// - while a replay is expected, output is held;
/// - a complete replay resets the terminal at its size, writes the snapshot,
///   and becomes the baseline; held output newer than it follows, the rest drops;
/// - after that, output at or below the baseline drops (outputs are never
///   compared with each other: the relay's chunks of one run share a `seq`);
/// - the latest replay always wins, even with a lower `seq`;
/// - a chunked replay renders only once whole; a gap or a malformed part asks
///   for a re-attach.
struct ReplayOrder {
    enum Frame: Equatable {
        case output(seq: UInt64, data: Data)
        /// An unchunked replay is part 0 of 1.
        case replay(seq: UInt64, cols: Int, rows: Int, part: Int = 0, parts: Int = 1, data: Data)
    }

    enum Action: Equatable {
        /// Replace the terminal with a fresh one at this size: screen, history,
        /// modes, selection and scroll position all go.
        case reset(cols: Int, rows: Int)
        case write(Data)
        /// The stream can't be trusted: re-send `flow.attach` (the order has
        /// already forgotten this attach and waits for the new replay).
        case resync(ResyncReason)
    }

    enum ResyncReason: String, Equatable {
        case replayGap = "replay_gap"
        case replayMalformed = "replay_malformed"
        case bufferOverflow = "buffer_overflow"
    }

    private struct Partial: Equatable {
        var seq: UInt64
        var cols: Int
        var rows: Int
        var parts: Int
        var next: Int
        var data: Data
    }

    /// The engine advertised `replay` and the attach asked for one. Against an
    /// older engine output applies in arrival order (a replay is still honoured).
    let expectReplay: Bool
    /// The most output held while a replay is awaited before giving up.
    let maxPendingBytes: Int
    private(set) var baseline: UInt64?
    private var partial: Partial?
    private var pending: [(seq: UInt64, data: Data)] = []
    private var pendingBytes = 0

    init(expectReplay: Bool, maxPendingBytes: Int = 4 * 1024 * 1024) {
        self.expectReplay = expectReplay
        self.maxPendingBytes = maxPendingBytes
    }

    /// True while output is being held for a replay.
    var isWaiting: Bool { partial != nil || (expectReplay && baseline == nil) }

    /// The client is (re)sending `flow.attach`: forget the baseline and
    /// anything held, and wait for the replay the attach brings.
    mutating func onAttach() {
        baseline = nil
        partial = nil
        pending = []
        pendingBytes = 0
    }

    /// One frame, in arrival order. Returns what to do, in order.
    mutating func onFrame(_ frame: Frame) -> [Action] {
        switch frame {
        case let .output(seq, data): onOutput(seq: seq, data: data)
        case let .replay(seq, cols, rows, part, parts, data): onReplay(seq: seq, cols: cols, rows: rows, part: part, parts: parts, data: data)
        }
    }

    private mutating func onOutput(seq: UInt64, data: Data) -> [Action] {
        if isWaiting {
            pendingBytes += data.count
            pending.append((seq, data))
            return pendingBytes > maxPendingBytes ? resync(.bufferOverflow) : []
        }
        if let b = baseline, seq <= b { return [] }
        return [.write(data)]
    }

    private mutating func onReplay(seq: UInt64, cols: Int, rows: Int, part: Int, parts: Int, data: Data) -> [Action] {
        if parts <= 0 || part < 0 || part >= parts { return resync(.replayMalformed) }
        if part == 0 {
            // A new replay; an incomplete one before it is superseded.
            partial = Partial(seq: seq, cols: cols, rows: rows, parts: parts, next: 1, data: data)
        } else {
            guard var p = partial, p.seq == seq, p.parts == parts, p.cols == cols, p.rows == rows, p.next == part else {
                return resync(.replayGap)
            }
            p.data.append(data)
            p.next += 1
            partial = p
        }
        guard let p = partial, p.next == p.parts else { return [] }
        partial = nil
        var out: [Action] = [.reset(cols: p.cols, rows: p.rows)]
        if !p.data.isEmpty { out.append(.write(p.data)) }
        baseline = p.seq
        out += pending.filter { $0.seq > p.seq }.map { .write($0.data) }
        pending = []
        pendingBytes = 0
        return out
    }

    private mutating func resync(_ reason: ResyncReason) -> [Action] {
        onAttach()
        return [.resync(reason)]
    }
}

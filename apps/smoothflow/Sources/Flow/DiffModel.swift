import Foundation

// SmoothFlow Diff (th-26f5b9): the `flow.diff` payload and the pure rules the
// viewer follows. The engine computes everything — hunks, word spans, syntax
// spans; this file only decodes it and decides layout. The rules mirror
// `crates/smooth-flow-client/src/diff.rs` and are pinned by
// `spec/vectors/diff.json` (ConformanceVectorTests.testDiff).

enum DiffBase: String, Codable, CaseIterable, Equatable {
    case turn, uncommitted, branch
}

enum DiffLineKind: String, Codable, Equatable {
    case add, del, ctx
}

struct DiffSide: Codable, Equatable {
    var ref: String
    var label: String
}

struct DiffTurn: Codable, Equatable {
    var seq: Int
    var live: Bool
    var startedAt: String?
    var endedAt: String?
    enum CodingKeys: String, CodingKey { case seq, live, startedAt = "started_at", endedAt = "ended_at" }
    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        seq = try c.decodeIfPresent(Int.self, forKey: .seq) ?? 0
        live = try c.decodeIfPresent(Bool.self, forKey: .live) ?? false
        startedAt = try c.decodeIfPresent(String.self, forKey: .startedAt)
        endedAt = try c.decodeIfPresent(String.self, forKey: .endedAt)
    }
}

struct DiffLine: Codable, Equatable {
    var kind: DiffLineKind
    var old: Int?
    var new: Int?
    var text: String
    var noEol: Bool
    var truncated: Bool
    /// `[start, end, kind]` in Unicode scalars; `kind` indexes `legend`.
    var syntax: [[Int]]
    /// `[start, end]` in Unicode scalars.
    var words: [[Int]]

    enum CodingKeys: String, CodingKey { case kind, old, new, text, noEol = "no_eol", truncated, syntax, words }

    init(kind: DiffLineKind, old: Int? = nil, new: Int? = nil, text: String = "", noEol: Bool = false, truncated: Bool = false,
         syntax: [[Int]] = [], words: [[Int]] = []) {
        self.kind = kind; self.old = old; self.new = new; self.text = text; self.noEol = noEol; self.truncated = truncated
        self.syntax = syntax; self.words = words
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        kind = try c.decodeIfPresent(DiffLineKind.self, forKey: .kind) ?? .ctx
        old = try c.decodeIfPresent(Int.self, forKey: .old)
        new = try c.decodeIfPresent(Int.self, forKey: .new)
        text = try c.decodeIfPresent(String.self, forKey: .text) ?? ""
        noEol = try c.decodeIfPresent(Bool.self, forKey: .noEol) ?? false
        truncated = try c.decodeIfPresent(Bool.self, forKey: .truncated) ?? false
        syntax = (try? c.decodeIfPresent([[Int]].self, forKey: .syntax)) ?? []
        words = (try? c.decodeIfPresent([[Int]].self, forKey: .words)) ?? []
    }
}

struct DiffHunk: Codable, Equatable {
    var id: String
    var oldStart: Int
    var oldLines: Int
    var newStart: Int
    var newLines: Int
    var section: String
    var lines: [DiffLine]
    var truncated: Bool
    var staged: Bool

    enum CodingKeys: String, CodingKey {
        case id, section, lines, truncated, staged
        case oldStart = "old_start", oldLines = "old_lines", newStart = "new_start", newLines = "new_lines"
    }

    init(id: String, oldStart: Int = 1, oldLines: Int = 0, newStart: Int = 1, newLines: Int = 0, section: String = "",
         lines: [DiffLine] = [], truncated: Bool = false, staged: Bool = false) {
        self.id = id; self.oldStart = oldStart; self.oldLines = oldLines; self.newStart = newStart; self.newLines = newLines
        self.section = section; self.lines = lines; self.truncated = truncated; self.staged = staged
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(String.self, forKey: .id)
        oldStart = try c.decodeIfPresent(Int.self, forKey: .oldStart) ?? 0
        oldLines = try c.decodeIfPresent(Int.self, forKey: .oldLines) ?? 0
        newStart = try c.decodeIfPresent(Int.self, forKey: .newStart) ?? 0
        newLines = try c.decodeIfPresent(Int.self, forKey: .newLines) ?? 0
        section = try c.decodeIfPresent(String.self, forKey: .section) ?? ""
        lines = try c.decodeIfPresent([DiffLine].self, forKey: .lines) ?? []
        truncated = try c.decodeIfPresent(Bool.self, forKey: .truncated) ?? false
        staged = try c.decodeIfPresent(Bool.self, forKey: .staged) ?? false
    }

    /// `@@ -a,b +c,d @@ section`
    var header: String {
        "@@ -\(oldStart),\(oldLines) +\(newStart),\(newLines) @@" + (section.isEmpty ? "" : " \(section)")
    }
}

struct DiffFile: Codable, Equatable {
    var path: String
    var oldPath: String?
    /// added | deleted | modified | renamed | copied | mode_changed
    var status: String
    var binary: Bool
    var language: String?
    var added: Int
    var deleted: Int
    /// lockfile | generated | vendored | minified | large
    var noise: String?
    var collapsedByDefault: Bool
    /// collapsed | budget — the hunks are not in this payload.
    var hunksOmitted: String?
    var truncated: Bool
    var hunks: [DiffHunk]

    enum CodingKeys: String, CodingKey {
        case path, status, binary, language, added, deleted, noise, truncated, hunks
        case oldPath = "old_path", collapsedByDefault = "collapsed_by_default", hunksOmitted = "hunks_omitted"
    }

    init(path: String, oldPath: String? = nil, status: String = "modified", binary: Bool = false, language: String? = nil,
         added: Int = 0, deleted: Int = 0, noise: String? = nil, collapsedByDefault: Bool = false, hunksOmitted: String? = nil,
         truncated: Bool = false, hunks: [DiffHunk] = []) {
        self.path = path; self.oldPath = oldPath; self.status = status; self.binary = binary; self.language = language
        self.added = added; self.deleted = deleted; self.noise = noise; self.collapsedByDefault = collapsedByDefault
        self.hunksOmitted = hunksOmitted; self.truncated = truncated; self.hunks = hunks
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        path = try c.decode(String.self, forKey: .path)
        oldPath = try c.decodeIfPresent(String.self, forKey: .oldPath)
        status = try c.decodeIfPresent(String.self, forKey: .status) ?? "modified"
        binary = try c.decodeIfPresent(Bool.self, forKey: .binary) ?? false
        language = try c.decodeIfPresent(String.self, forKey: .language)
        added = try c.decodeIfPresent(Int.self, forKey: .added) ?? 0
        deleted = try c.decodeIfPresent(Int.self, forKey: .deleted) ?? 0
        noise = try c.decodeIfPresent(String.self, forKey: .noise)
        collapsedByDefault = try c.decodeIfPresent(Bool.self, forKey: .collapsedByDefault) ?? false
        hunksOmitted = try c.decodeIfPresent(String.self, forKey: .hunksOmitted)
        truncated = try c.decodeIfPresent(Bool.self, forKey: .truncated) ?? false
        hunks = try c.decodeIfPresent([DiffHunk].self, forKey: .hunks) ?? []
    }

    /// The one-letter badge the tree shows.
    var badge: String {
        switch status {
        case "added": "A"
        case "deleted": "D"
        case "renamed": "R"
        case "copied": "C"
        case "mode_changed": "X"
        default: "M"
        }
    }
}

struct DiffPayload: Codable, Equatable {
    var base: DiffBase
    var from: DiffSide
    var to: DiffSide
    var turn: DiffTurn?
    var note: String?
    var files: [DiffFile]
    var added: Int
    var deleted: Int
    var truncated: Bool
    var filesOmitted: Int
    var legend: [String]

    enum CodingKeys: String, CodingKey { case base, from, to, turn, note, files, added, deleted, truncated, legend, filesOmitted = "files_omitted" }

    init(base: DiffBase, files: [DiffFile] = [], note: String? = nil) {
        self.base = base; from = DiffSide(ref: "", label: ""); to = DiffSide(ref: "", label: ""); turn = nil; self.note = note
        self.files = files; added = files.reduce(0) { $0 + $1.added }; deleted = files.reduce(0) { $0 + $1.deleted }
        truncated = false; filesOmitted = 0; legend = []
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        base = try c.decodeIfPresent(DiffBase.self, forKey: .base) ?? .uncommitted
        from = try c.decodeIfPresent(DiffSide.self, forKey: .from) ?? DiffSide(ref: "", label: "")
        to = try c.decodeIfPresent(DiffSide.self, forKey: .to) ?? DiffSide(ref: "", label: "")
        turn = try c.decodeIfPresent(DiffTurn.self, forKey: .turn)
        note = try c.decodeIfPresent(String.self, forKey: .note)
        files = try c.decodeIfPresent([DiffFile].self, forKey: .files) ?? []
        added = try c.decodeIfPresent(Int.self, forKey: .added) ?? 0
        deleted = try c.decodeIfPresent(Int.self, forKey: .deleted) ?? 0
        truncated = try c.decodeIfPresent(Bool.self, forKey: .truncated) ?? false
        filesOmitted = try c.decodeIfPresent(Int.self, forKey: .filesOmitted) ?? 0
        legend = try c.decodeIfPresent([String].self, forKey: .legend) ?? []
    }
}

/// `flow.diff.result`.
struct DiffResult: Equatable {
    var id: String
    var action: String
    var hunkId: String?
    var file: String?
    var message: String?
}

/// One review comment, kept client-side until "Send review".
struct DiffComment: Equatable, Identifiable {
    var id = UUID()
    var file: String
    var hunkId: String?
    /// Inclusive line numbers on `side`.
    var range: ClosedRange<Int>?
    /// `new` or `old`.
    var side: String = "new"
    var text: String

    var fields: [String: Any] {
        var o: [String: Any] = ["file": file, "text": text, "side": side]
        if let hunkId { o["hunk_id"] = hunkId }
        if let range { o["line_range"] = [range.lowerBound, range.upperBound] }
        return o
    }
}

// MARK: - rules (spec §14)

enum DiffRules {
    struct Row: Equatable { var left: Int?; var right: Int? }

    /// Side-by-side rows of one hunk: context fills both columns; in a change
    /// block the i-th del sits beside the i-th add, the rest get blanks.
    static func sideBySide(_ kinds: [DiffLineKind]) -> [Row] {
        var rows: [Row] = []
        var i = 0
        while i < kinds.count {
            if kinds[i] == .ctx {
                rows.append(Row(left: i, right: i))
                i += 1
                continue
            }
            let delStart = i
            while i < kinds.count, kinds[i] == .del { i += 1 }
            let addStart = i
            while i < kinds.count, kinds[i] == .add { i += 1 }
            let dels = addStart - delStart, adds = i - addStart
            for k in 0..<max(dels, adds) {
                rows.append(Row(left: k < dels ? delStart + k : nil, right: k < adds ? addStart + k : nil))
            }
        }
        return rows
    }

    struct TreeRow: Equatable {
        var kind: String
        var name: String
        var path: String
        var depth: Int
        var file: Int?
    }

    private final class Dir {
        let name: String
        var dirs: [Dir] = []
        var files: [(String, Int)] = []
        init(_ name: String) { self.name = name }

        func insert(_ parts: ArraySlice<String>, _ idx: Int) {
            guard let first = parts.first else { return }
            if parts.count == 1 { files.append((first, idx)); return }
            let d = dirs.first { $0.name == first } ?? { let n = Dir(first); dirs.append(n); return n }()
            d.insert(parts.dropFirst(), idx)
        }

        static func less(_ a: String, _ b: String) -> Bool {
            let (la, lb) = (a.lowercased(), b.lowercased())
            // Byte order, like Rust's String Ord — not localized compare.
            return la != lb ? Array(la.utf8).lexicographicallyPrecedes(Array(lb.utf8)) : Array(a.utf8).lexicographicallyPrecedes(Array(b.utf8))
        }

        func flatten(_ prefix: String, _ depth: Int, into out: inout [TreeRow]) {
            for d in dirs.sorted(by: { Dir.less($0.name, $1.name) }) {
                var name = d.name
                var node = d
                while node.files.isEmpty, node.dirs.count == 1 {
                    node = node.dirs[0]
                    name += "/" + node.name
                }
                let path = prefix.isEmpty ? name : prefix + "/" + name
                out.append(TreeRow(kind: "dir", name: name, path: path, depth: depth, file: nil))
                node.flatten(path, depth + 1, into: &out)
            }
            for (n, idx) in files.sorted(by: { Dir.less($0.0, $1.0) }) {
                out.append(TreeRow(kind: "file", name: n, path: prefix.isEmpty ? n : prefix + "/" + n, depth: depth, file: idx))
            }
        }
    }

    /// The file tree: directories first, case-insensitive, chains compressed.
    static func tree(_ paths: [String]) -> [TreeRow] {
        let root = Dir("")
        for (i, p) in paths.enumerated() { root.insert(ArraySlice(p.split(separator: "/").map(String.init)), i) }
        var out: [TreeRow] = []
        root.flatten("", 0, into: &out)
        return out
    }

    static func fileOrder(_ paths: [String]) -> [Int] { tree(paths).compactMap(\.file) }

    struct Display: Equatable { var collapsed: Bool; var reason: String?; var fetch: Bool }

    static func display(_ f: DiffFile, viewed: Bool) -> Display {
        let reason: String? = if viewed { "viewed" }
        else if f.collapsedByDefault || f.noise != nil { "noise" }
        else if f.binary { "binary" }
        else if f.hunks.isEmpty, f.hunksOmitted == nil { "no_content" }
        else { nil }
        return Display(collapsed: reason != nil, reason: reason, fetch: f.hunksOmitted != nil)
    }

    /// The key a viewed mark is stored under; it changes when the file's change does.
    static func viewedKey(_ f: DiffFile) -> String {
        f.hunks.isEmpty ? "\(f.path)|\(f.status)|+\(f.added)-\(f.deleted)" : "\(f.path)|" + f.hunks.map(\.id).joined(separator: ",")
    }

    typealias Pos = (file: Int, hunk: Int)

    static func nextHunk(hunkCounts: [Int], order: [Int], collapsed: [Bool], at: Pos?, forward: Bool) -> Pos? {
        let all: [Pos] = order.filter { !(collapsed.indices.contains($0) && collapsed[$0]) }
            .flatMap { f in (0..<(hunkCounts.indices.contains(f) ? hunkCounts[f] : 0)).map { (file: f, hunk: $0) } }
        let here = at.flatMap { p in all.firstIndex { $0.file == p.file && $0.hunk == p.hunk } }
        switch (here, forward) {
        case (nil, true): return all.first
        case (nil, false): return all.last
        case let (i?, true): return i + 1 < all.count ? all[i + 1] : nil
        case let (i?, false): return i > 0 ? all[i - 1] : nil
        }
    }

    static func nextFile(order: [Int], at: Int?, forward: Bool) -> Int? {
        let here = at.flatMap { order.firstIndex(of: $0) }
        switch (here, forward) {
        case (nil, true): return order.first
        case (nil, false): return order.last
        case let (i?, true): return i + 1 < order.count ? order[i + 1] : nil
        case let (i?, false): return i > 0 ? order[i - 1] : nil
        }
    }

    static func defaultBase(kind: String) -> DiffBase { kind == "shell" ? .uncommitted : .turn }

    static func branchRef(fromLabel label: String) -> String? {
        label.hasPrefix("merge base with ") ? String(label.dropFirst("merge base with ".count)).trimmingCharacters(in: .whitespaces) : nil
    }

    static func baseLabel(_ base: DiffBase, branchRef: String?) -> String {
        switch base {
        case .turn: return "Last turn"
        case .uncommitted: return "Uncommitted"
        case .branch:
            if let r = branchRef?.split(separator: "/").last.map(String.init), !r.isEmpty, r != "HEAD" { return "vs \(r)" }
            return "vs default branch"
        }
    }
}

// MARK: - text

enum DiffText {
    /// Scalar offset → UTF-16 offset table for one line (count+1 entries).
    static func utf16Offsets(_ text: String) -> [Int] {
        var out = [0]
        out.reserveCapacity(text.unicodeScalars.count + 1)
        var n = 0
        for s in text.unicodeScalars {
            n += s.utf16.count
            out.append(n)
        }
        return out
    }

    /// The NSRange of scalar span `[start, end)`, clamped to the line.
    static func nsRange(_ offsets: [Int], _ start: Int, _ end: Int) -> NSRange? {
        let last = offsets.count - 1
        let s = min(max(start, 0), last), e = min(max(end, 0), last)
        guard e > s else { return nil }
        return NSRange(location: offsets[s], length: offsets[e] - offsets[s])
    }
}

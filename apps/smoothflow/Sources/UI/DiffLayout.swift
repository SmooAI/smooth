import AppKit

/// One row of the Diff viewer's main table. Rows are computed, never views:
/// the table is virtualized, so only what is on screen gets a cell.
enum DiffRow: Equatable {
    case banner(String)
    case fileHeader(file: Int)
    /// Why a file shows no lines, and whether "Show" fetches its hunks.
    case notice(file: Int, text: String, fetch: Bool)
    case hunkHeader(file: Int, hunk: Int)
    /// Unified mode: one line.
    case line(file: Int, hunk: Int, line: Int)
    /// Side-by-side mode: indexes into the hunk's lines for each column.
    case pair(file: Int, hunk: Int, left: Int?, right: Int?)
    /// A pending review comment, under the line it is about.
    case comment(file: Int, index: Int)

    var file: Int? {
        switch self {
        case .banner: nil
        case let .fileHeader(f), let .notice(f, _, _), let .hunkHeader(f, _), let .line(f, _, _), let .pair(f, _, _, _), let .comment(f, _): f
        }
    }

    var hunk: Int? {
        switch self {
        case let .hunkHeader(_, h), let .line(_, h, _), let .pair(_, h, _, _): h
        default: nil
        }
    }

    /// A row that j/k stop on.
    var isLine: Bool {
        switch self {
        case .line, .pair: true
        default: false
        }
    }
}

/// Pure layout of the Diff viewer (XCTested): which rows, in what order.
enum DiffLayout {
    /// Notice text for a collapsed or content-less file.
    static func noticeText(_ f: DiffFile, _ d: DiffRules.Display) -> String {
        switch d.reason {
        case "viewed": return "Viewed"
        case "noise":
            let what = switch f.noise ?? "" {
            case "lockfile": "Lockfile"
            case "vendored": "Vendored code"
            case "minified": "Minified file"
            case "large": "Large change"
            default: "Generated file"
            }
            return "\(what) · +\(f.added) −\(f.deleted) · collapsed"
        case "binary": return "Binary file"
        case "no_content":
            if f.status == "renamed" { return "Renamed from \(f.oldPath ?? "?") with no content change" }
            if f.status == "mode_changed" { return "File mode changed" }
            return "No content change"
        default: return f.hunksOmitted == "budget" ? "Large diff · +\(f.added) −\(f.deleted) · not loaded yet" : ""
        }
    }

    /// The comment's anchor line in `file`: the last line of its range on its side.
    static func anchors(_ comment: DiffComment, in f: DiffFile, hunk: Int, line: Int) -> Bool {
        guard comment.file == f.path, let range = comment.range else { return false }
        if let id = comment.hunkId, f.hunks[hunk].id != id { return false }
        let l = f.hunks[hunk].lines[line]
        let n = comment.side == "old" ? l.old : l.new
        return n == range.upperBound
    }

    /// All rows. `collapsed[i]` is file i's effective state (rules +
    /// user toggles); files appear in tree order.
    static func rows(_ diff: DiffPayload, split: Bool, collapsed: [Bool], viewed: [Bool], comments: [DiffComment]) -> [DiffRow] {
        var out: [DiffRow] = []
        if let note = diff.note, !note.isEmpty { out.append(.banner(note)) }
        if diff.truncated {
            out.append(.banner("\(diff.filesOmitted) more file\(diff.filesOmitted == 1 ? "" : "s") not shown — the diff is too large to send whole."))
        }
        let order = DiffRules.fileOrder(diff.files.map(\.path))
        for fi in order {
            let f = diff.files[fi]
            out.append(.fileHeader(file: fi))
            // File-level comments (no range) sit under the header.
            for (ci, c) in comments.enumerated() where c.file == f.path && c.range == nil {
                out.append(.comment(file: fi, index: ci))
            }
            let rule = DiffRules.display(f, viewed: viewed.indices.contains(fi) && viewed[fi])
            let isCollapsed = collapsed.indices.contains(fi) ? collapsed[fi] : rule.collapsed
            if isCollapsed {
                out.append(.notice(file: fi, text: noticeText(f, rule), fetch: rule.fetch))
                continue
            }
            if f.hunks.isEmpty {
                // Expanded, but nothing inline: hunks still to fetch, or none exist.
                let text = rule.fetch ? "+\(f.added) −\(f.deleted) · loading…" : noticeText(f, DiffRules.Display(collapsed: true, reason: "no_content", fetch: false))
                out.append(.notice(file: fi, text: text, fetch: rule.fetch))
                continue
            }
            for (hi, h) in f.hunks.enumerated() {
                out.append(.hunkHeader(file: fi, hunk: hi))
                if split {
                    for r in DiffRules.sideBySide(h.lines.map(\.kind)) {
                        out.append(.pair(file: fi, hunk: hi, left: r.left, right: r.right))
                        for (ci, c) in comments.enumerated() {
                            let hit = [r.left, r.right].compactMap { $0 }.contains { anchors(c, in: f, hunk: hi, line: $0) }
                            if hit { out.append(.comment(file: fi, index: ci)) }
                        }
                    }
                } else {
                    for li in h.lines.indices {
                        out.append(.line(file: fi, hunk: hi, line: li))
                        for (ci, c) in comments.enumerated() where anchors(c, in: f, hunk: hi, line: li) {
                            out.append(.comment(file: fi, index: ci))
                        }
                    }
                }
                if h.truncated { out.append(.notice(file: fi, text: "Hunk truncated — the rest of it is too large to show", fetch: false)) }
            }
            if f.truncated, !f.hunks.contains(where: \.truncated) {
                out.append(.notice(file: fi, text: "File truncated — more changes than the viewer shows", fetch: false))
            }
        }
        return out
    }
}

/// Catppuccin Mocha, for the engine's token kinds (by legend name).
enum DiffPalette {
    static func hex(_ v: UInt32, _ a: CGFloat = 1) -> NSColor {
        NSColor(srgbRed: CGFloat((v >> 16) & 0xff) / 255, green: CGFloat((v >> 8) & 0xff) / 255, blue: CGFloat(v & 0xff) / 255, alpha: a)
    }

    static let base = hex(0x1e1e2e)
    static let mantle = hex(0x181825)
    static let crust = hex(0x11111b)
    static let surface0 = hex(0x313244)
    static let surface1 = hex(0x45475a)
    static let overlay0 = hex(0x6c7086)
    static let overlay2 = hex(0x9399b2)
    static let subtext = hex(0xa6adc8)
    static let text = hex(0xcdd6f4)
    static let green = hex(0xa6e3a1)
    static let red = hex(0xf38ba8)
    static let mauve = hex(0xcba6f7)
    static let peach = hex(0xfab387)
    static let yellow = hex(0xf9e2af)
    static let blue = hex(0x89b4fa)
    static let sky = hex(0x89dceb)
    static let lavender = hex(0xb4befe)
    static let pink = hex(0xf5c2e7)
    static let rosewater = hex(0xf5e0dc)
    static let teal = hex(0x94e2d5)

    static let addLine = hex(0xa6e3a1, 0.10)
    static let delLine = hex(0xf38ba8, 0.10)
    static let addWord = hex(0xa6e3a1, 0.32)
    static let delWord = hex(0xf38ba8, 0.32)
    static let cursor = hex(0x89b4fa, 0.16)
    static let selected = hex(0x89b4fa, 0.10)

    /// The color for a token kind name, nil for plain text.
    static func color(forToken name: String) -> NSColor? {
        switch name {
        case "keyword": mauve
        case "string": green
        case "comment": overlay2
        case "number", "constant": peach
        case "function": blue
        case "type": yellow
        case "variable": text
        case "property": lavender
        case "operator": sky
        case "punctuation": overlay2
        case "tag": blue
        case "attribute": yellow
        case "macro": rosewater
        case "escape": pink
        case "heading": red
        case "link": rosewater
        default: nil
        }
    }

    static func badgeColor(_ status: String) -> NSColor {
        switch status {
        case "added": green
        case "deleted": red
        case "renamed", "copied": blue
        case "mode_changed": overlay2
        default: peach
        }
    }
}

/// Attributed text for one diff line: syntax colors from the engine's spans,
/// word-level backgrounds from its word spans. Pure apart from fonts.
enum DiffStyler {
    static func line(_ l: DiffLine, legend: [String], font: NSFont) -> NSAttributedString {
        let s = NSMutableAttributedString(string: l.text, attributes: [.font: font, .foregroundColor: DiffPalette.text])
        let offsets = DiffText.utf16Offsets(l.text)
        for span in l.syntax where span.count == 3 {
            guard legend.indices.contains(span[2]), let color = DiffPalette.color(forToken: legend[span[2]]),
                  let r = DiffText.nsRange(offsets, span[0], span[1]) else { continue }
            s.addAttribute(.foregroundColor, value: color, range: r)
        }
        let wordBg = l.kind == .add ? DiffPalette.addWord : DiffPalette.delWord
        for span in l.words where span.count == 2 {
            guard l.kind != .ctx, let r = DiffText.nsRange(offsets, span[0], span[1]) else { continue }
            s.addAttribute(.backgroundColor, value: wordBg, range: r)
        }
        if l.truncated {
            s.append(NSAttributedString(string: " …", attributes: [.font: font, .foregroundColor: DiffPalette.overlay0]))
        }
        if l.noEol {
            s.append(NSAttributedString(string: " ⏎̸", attributes: [.font: font, .foregroundColor: DiffPalette.red]))
        }
        return s
    }
}

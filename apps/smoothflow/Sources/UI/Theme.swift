import AppKit
import SwiftUI

/// Presence palette from the wireframes: color is spent on state, not decoration.
enum Theme {
    static let teal = NSColor(srgbRed: 0x0e / 255, green: 0x8c / 255, blue: 0x93 / 255, alpha: 1)
    static let amber = NSColor(srgbRed: 0xc9 / 255, green: 0x84 / 255, blue: 0x0f / 255, alpha: 1)
    static let ink = NSColor(srgbRed: 0x1f / 255, green: 0x29 / 255, blue: 0x33 / 255, alpha: 1)
    static let muted = NSColor(srgbRed: 0x5a / 255, green: 0x6b / 255, blue: 0x76 / 255, alpha: 1)
    static let faint = NSColor(srgbRed: 0x9a / 255, green: 0xa5 / 255, blue: 0xad / 255, alpha: 1)
    static let amberWash = NSColor(srgbRed: 0xff / 255, green: 0xf4 / 255, blue: 0xdd / 255, alpha: 1)
    static let blue = NSColor(srgbRed: 0x2a / 255, green: 0x5f / 255, blue: 0xd0 / 255, alpha: 1)

    /// The app's monospace text: the bundled terminal family when it
    /// registered, so chrome and panes match (th-bcd819); else the system
    /// monospace design.
    static func mono(_ style: Font.TextStyle) -> Font {
        guard TerminalFont.registeredFamily != nil else { return .system(style, design: .monospaced) }
        return .custom(TerminalFont.bundledPostScriptRegular, size: NSFont.preferredFont(forTextStyle: style.nsTextStyle).pointSize, relativeTo: style)
    }

    /// AppKit twin of `mono` for NSTextField / NSTextView.
    static func monoNSFont(size: CGFloat) -> NSFont {
        (TerminalFont.registeredFamily != nil ? NSFont(name: TerminalFont.bundledPostScriptRegular, size: size) : nil) ?? .monospacedSystemFont(ofSize: size, weight: .regular)
    }

    /// Sidebar dot: teal working, amber needs you / limited, ink done, grey idle.
    static func dot(for s: Session) -> Color {
        switch s.state {
        case .working, .starting: Color(teal)
        case .needsYou, .limited: Color(amber)
        case .done: Color(ink)
        case .dead: Color(.systemRed)
        case .idle: s.attention?.reason == .held ? Color(amber) : Color(faint)
        case .unknown: Color(faint)
        }
    }

    static func stateLabel(_ s: Session) -> String {
        switch s.state {
        case .starting: "starting"
        case .working: "working"
        case .idle: s.attention?.reason == .held ? "held" : "idle"
        case .needsYou: s.attention?.reason == .question ? "question" : "approve"
        case .limited: "limit" + (s.attention?.resumeDate.map { " · \(AttentionNotifier.timeFormatter.string(from: $0))" } ?? "")
        case .done: "done"
        case .dead: "dead"
        case .unknown: "?"
        }
    }

    /// A wall-clock time for an RFC3339 stamp ("3:42 PM"), or the raw string.
    static func clock(_ iso: String) -> String {
        guard let d = ISO8601DateFormatter.flexible.date(from: iso) else { return iso }
        return AttentionNotifier.timeFormatter.string(from: d)
    }

    static func relative(_ iso: String) -> String {
        guard let d = ISO8601DateFormatter.flexible.date(from: iso) else { return "" }
        let secs = Int(-d.timeIntervalSinceNow)
        if secs < 60 { return "\(secs)s ago" }
        if secs < 3600 { return "\(secs / 60)m ago" }
        return "\(secs / 3600)h ago"
    }
}

struct StateDot: View {
    let session: Session
    var body: some View {
        Circle().fill(Theme.dot(for: session)).frame(width: 9, height: 9)
    }
}

struct UnreadBadge: View {
    var body: some View {
        Circle().fill(Color(Theme.amber)).frame(width: 7, height: 7)
    }
}

extension Font.TextStyle {
    /// The AppKit text style with the same default point size.
    var nsTextStyle: NSFont.TextStyle {
        switch self {
        case .largeTitle: .largeTitle
        case .title: .title1
        case .title2: .title2
        case .title3: .title3
        case .headline: .headline
        case .subheadline: .subheadline
        case .callout: .callout
        case .footnote: .footnote
        case .caption: .caption1
        case .caption2: .caption2
        default: .body
        }
    }
}

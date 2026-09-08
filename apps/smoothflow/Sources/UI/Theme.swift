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

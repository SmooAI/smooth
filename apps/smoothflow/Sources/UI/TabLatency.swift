import AppKit
import os

/// SMOODEV-4127: how long a tab action (⌘T, ⌘N, ⌘W, a click, a middle-click)
/// takes to reach the screen. `log stream --predicate 'subsystem ==
/// "ai.smoo.smoothflow" AND category == "latency"' --level debug` shows it.
@MainActor
enum TabLatency {
    static let log = Logger(subsystem: "ai.smoo.smoothflow", category: "latency")

    /// Run `body` for `action` and log, once the run loop has committed the
    /// frame it produced: how long the event waited before it was handled,
    /// how long the handler ran, and the total from the event to that commit.
    static func measure(_ action: String, _ body: () -> Void) {
        let t0 = ProcessInfo.processInfo.systemUptime
        let queued = NSApp.currentEvent.map { max(0, t0 - $0.timestamp) } ?? 0
        body()
        let ran = ProcessInfo.processInfo.systemUptime - t0
        // The next turn of the main queue runs after this turn's layout and
        // Core Animation commit: what the window shows next.
        DispatchQueue.main.async {
            let shown = ProcessInfo.processInfo.systemUptime - t0
            log.info(
                "tab \(action, privacy: .public): queued \(ms(queued), privacy: .public) ms, handler \(ms(ran), privacy: .public) ms, on screen \(ms(queued + shown), privacy: .public) ms"
            )
        }
    }

    nonisolated static func ms(_ seconds: TimeInterval) -> String { String(format: "%.1f", seconds * 1000) }

    /// Per-stage times inside one render.
    struct Stages {
        private var last = ProcessInfo.processInfo.systemUptime
        private var parts: [String] = []

        mutating func mark(_ name: String) {
            let now = ProcessInfo.processInfo.systemUptime
            parts.append("\(name) \(TabLatency.ms(now - last))")
            last = now
        }

        var summary: String { parts.joined(separator: ", ") }
    }
}

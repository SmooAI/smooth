import Foundation

/// What a ⌘W empties. The pane always goes; the scope names the container that
/// collapses with it, which is what the prompt has to be honest about.
enum PaneCloseScope: String, Equatable, Sendable {
    /// One of several panes in the tab.
    case pane
    /// The last pane in a tab that is not the last tab.
    case tab
    /// The last pane of the last tab — the window goes.
    case window

    var noun: String {
        switch self {
        case .pane: "Pane"
        case .tab: "Tab"
        case .window: "Window"
        }
    }
}

/// Whether ⌘W may act at once, and what to ask if not.
///
/// This is deliberately a pure function of the pane's session, separate from
/// the `NSAlert` that presents it: "does this need confirming, and what does it
/// say" is the part with rules in it, and the part worth testing.
struct PaneCloseDecision: Equatable {
    var scope: PaneCloseScope
    /// nil — close without asking.
    var prompt: Prompt?

    struct Prompt: Equatable {
        var title: String
        var message: String
        /// The safe answer: the view goes, the session keeps running in the fleet.
        var closeTitle: String
        /// The destructive answer, when there is a live process worth offering
        /// to end. nil leaves the alert a two-button close/cancel.
        var killTitle: String?
    }
}

enum PaneClose {
    /// SmoothFlow panes are **views over engine-owned sessions**, not the
    /// sessions themselves — a tmux session outlives this window, and the
    /// sidebar still lists it. So ⌘W has two honest answers when a live session
    /// is on screen, and the alert offers both rather than guessing: close the
    /// view, or end the session. Cancel is the default button; the caller wires
    /// that up, because a stray Return must never kill an agent.
    ///
    /// Nothing is asked when there is nothing to lose:
    /// - an empty pane,
    /// - a session that has already finished or died (the pane is scrollback),
    /// - a **shell sitting at a prompt** — Ghostty's `confirm-close-surface`
    ///   draws exactly this line, and asking about an idle shell is the kind of
    ///   dialog people learn to dismiss without reading,
    /// - a session **still on screen in another pane or tab** — closing one of
    ///   two views of the same session loses nothing, and asking would make
    ///   ⌘T-then-⌘W (which starts on the focused session) feel booby-trapped,
    /// - or the user having turned the confirmation off, in which case ⌘W
    ///   closes the view and never kills anything.
    static func decide(session: Session?, harnessLabel: String, scope: PaneCloseScope,
                       shownElsewhere: Bool = false, confirmEnabled: Bool = true) -> PaneCloseDecision {
        let closeTitle = "Close \(scope.noun)"
        guard confirmEnabled, !shownElsewhere, let session, session.isLive, hasRunningProcess(session) else {
            return PaneCloseDecision(scope: scope, prompt: nil)
        }
        let name = session.pearlId ?? (session.title.isEmpty ? session.id : session.title)
        let label = harnessLabel.isEmpty ? session.kind : harnessLabel
        let title: String
        switch scope {
        case .pane: title = "Close this pane?"
        case .tab: title = "Close this tab?"
        case .window: title = "Close this window?"
        }
        let message: String
        if session.kind == "shell" {
            message = "Something is still running in \(name). Closing the \(scope.noun.lowercased()) leaves it running — you can get back to it from the sidebar. Ending the session kills the process."
        } else {
            message = "\(label) is \(activity(session)) on \(name). Closing the \(scope.noun.lowercased()) leaves it running — you can get back to it from the sidebar. "
                + "Ending the session kills the process, and an agent killed mid-turn loses the work in flight."
        }
        return PaneCloseDecision(scope: scope, prompt: .init(title: title, message: message, closeTitle: closeTitle, killTitle: "End Session"))
    }

    /// A live session with something actually running in it. For a harness this
    /// is any live state — an agent at rest still holds its context, and losing
    /// that is the thing people mind. For a shell it is a foreground process:
    /// `idle` is the engine's word for "at a prompt".
    static func hasRunningProcess(_ session: Session) -> Bool {
        guard session.isLive else { return false }
        guard session.kind == "shell" else { return true }
        return session.state != .idle
    }

    private static func activity(_ session: Session) -> String {
        switch session.state {
        case .working, .starting: "still working"
        case .needsYou: "waiting on you"
        case .limited: "paused on a usage limit"
        default: "open"
        }
    }
}

/// Whether ⌘W asks before closing a pane that holds a live session. A real
/// setting (Settings ▸ Terminal), so the alert may offer "Don't ask again".
enum PaneCloseSettings {
    static let key = "terminal.confirmClosePane"

    static func confirm(_ d: UserDefaults = .standard) -> Bool {
        d.object(forKey: key) == nil ? true : d.bool(forKey: key)
    }

    static func setConfirm(_ value: Bool, _ d: UserDefaults = .standard) { d.set(value, forKey: key) }
}

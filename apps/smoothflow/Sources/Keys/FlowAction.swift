import Foundation

/// Every keyboard-reachable action in SmoothFlow, and the shortcut it ships
/// with. This enum is the single source of truth: the menu bar is built from
/// it, `~/.smooth/smoothflow/keybindings.toml` overrides it by `rawValue`, and
/// Settings ▸ Keyboard lists it row by row. Adding an action here is the whole
/// job — nothing else has to be told about it.
enum FlowAction: String, CaseIterable, Identifiable, Sendable {
    // Session
    case newSession
    case newShell
    case fanOut
    case steerFocused
    case steerAll
    case allow
    case deny
    case killResume
    case kill
    case focusSession1, focusSession2, focusSession3, focusSession4, focusSession5
    case focusSession6, focusSession7, focusSession8, focusSession9

    // Tabs
    case newTab
    case closeTab
    case previousTab
    case nextTab

    // Splits
    case splitRight
    case splitDown
    case splitLeft
    case splitUp
    case focusPaneLeft
    case focusPaneRight
    case focusPaneUp
    case focusPaneDown
    case zoomPane
    case equalizePanes
    case closeSplit

    // View
    case inbox
    case viewTerminal
    case viewDiff
    case viewPR
    case viewActivity
    case toggleSidebar
    case togglePearlRail
    case settings

    var id: String { rawValue }

    enum Category: String, CaseIterable, Identifiable, Sendable {
        case session = "Session"
        case tabs = "Tabs"
        case splits = "Splits"
        case view = "View"
        var id: String { rawValue }
    }

    var category: Category {
        switch self {
        case .newSession, .newShell, .fanOut, .steerFocused, .steerAll, .allow, .deny, .killResume, .kill,
             .focusSession1, .focusSession2, .focusSession3, .focusSession4, .focusSession5,
             .focusSession6, .focusSession7, .focusSession8, .focusSession9:
            .session
        case .newTab, .closeTab, .previousTab, .nextTab:
            .tabs
        case .splitRight, .splitDown, .splitLeft, .splitUp,
             .focusPaneLeft, .focusPaneRight, .focusPaneUp, .focusPaneDown,
             .zoomPane, .equalizePanes, .closeSplit:
            .splits
        case .inbox, .viewTerminal, .viewDiff, .viewPR, .viewActivity, .toggleSidebar, .togglePearlRail, .settings:
            .view
        }
    }

    /// Menu-bar title. Focus-session rows are numbered from one enum case each
    /// so the file can rebind any single one of them.
    var title: String {
        switch self {
        case .newSession: return "New Session…"
        case .newShell: return "New Shell Here"
        case .fanOut: return "Fan Out…"
        case .steerFocused: return "Steer Focused…"
        case .steerAll: return "Steer All Working"
        case .allow: return "Approve (Allow)"
        case .deny: return "Deny"
        case .killResume: return "Kill & Resume"
        case .kill: return "Kill"
        case .newTab: return "New Tab"
        case .closeTab: return "Close Tab"
        case .previousTab: return "Previous Tab"
        case .nextTab: return "Next Tab"
        case .splitRight: return "Split Right"
        case .splitDown: return "Split Down"
        case .splitLeft: return "Split Left"
        case .splitUp: return "Split Up"
        case .focusPaneLeft: return "Focus Pane Left"
        case .focusPaneRight: return "Focus Pane Right"
        case .focusPaneUp: return "Focus Pane Up"
        case .focusPaneDown: return "Focus Pane Down"
        case .zoomPane: return "Zoom Pane"
        case .equalizePanes: return "Equalize Panes"
        case .closeSplit: return "Close Split"
        case .inbox: return "Inbox"
        case .viewTerminal: return "Terminal"
        case .viewDiff: return "Diff"
        case .viewPR: return "PR"
        case .viewActivity: return "Activity"
        case .toggleSidebar: return "Toggle Sidebar"
        case .togglePearlRail: return "Toggle Pearl Rail"
        case .settings: return "Settings…"
        default:
            if let n = Self.focusSessionIndex(self) { return "Focus Session \(n + 1)" }
            return rawValue
        }
    }

    /// 0-based fleet index for the nine focus actions, else nil.
    static func focusSessionIndex(_ a: FlowAction) -> Int? {
        guard a.rawValue.hasPrefix("focusSession"), let n = Int(a.rawValue.dropFirst("focusSession".count)) else { return nil }
        return n - 1
    }

    /// The shipped binding. `nil` means the action has no default shortcut and
    /// is menu-only until someone binds it.
    ///
    /// Two deliberate departures from what SmoothFlow shipped before (both
    /// rebindable in Settings ▸ Keyboard):
    ///
    /// - **Steer All Working moves ⌘⇧↩ → ⌘⌥↩.** ⌘⇧↩ is zoom-the-pane in
    ///   Ghostty, iTerm, tmux-with-a-prefix and cmux, and a terminal fleet
    ///   console that disagrees with every terminal is the one that is wrong.
    ///   ⌘⌥ is already the fleet-action family here (⌘⌥Y allow, ⌘⌥N deny,
    ///   ⌘⌥R kill & resume, ⌘⌥K kill), and steering every working session is
    ///   exactly a fleet action.
    /// - **⌘D is Split Right and ⌘⇧D is Split Down**, matching cmux/Ghostty,
    ///   rather than one untyped "Split Surface". The other two directions are
    ///   ⌘⇧← and ⌘⇧↑; ⌘⇧→ / ⌘⇧↓ are left free for anyone who wants the
    ///   symmetric four-arrow set instead of ⌘D/⌘⇧D.
    var defaultChord: KeyChord? {
        switch self {
        case .newSession: return KeyChord("n", command: true)
        case .newShell: return KeyChord("t", command: true, shift: true)
        case .fanOut: return KeyChord("n", command: true, shift: true)
        case .steerFocused: return KeyChord("enter", command: true)
        case .steerAll: return KeyChord("enter", command: true, option: true)
        case .allow: return KeyChord("y", command: true, option: true)
        case .deny: return KeyChord("n", command: true, option: true)
        case .killResume: return KeyChord("r", command: true, option: true)
        case .kill: return KeyChord("k", command: true, option: true)

        case .newTab: return KeyChord("t", command: true)
        case .closeTab: return KeyChord("w", command: true)
        case .previousTab: return KeyChord("[", command: true, shift: true)
        case .nextTab: return KeyChord("]", command: true, shift: true)

        case .splitRight: return KeyChord("d", command: true)
        case .splitDown: return KeyChord("d", command: true, shift: true)
        case .splitLeft: return KeyChord("left", command: true, shift: true)
        case .splitUp: return KeyChord("up", command: true, shift: true)
        case .focusPaneLeft: return KeyChord("left", command: true, option: true)
        case .focusPaneRight: return KeyChord("right", command: true, option: true)
        case .focusPaneUp: return KeyChord("up", command: true, option: true)
        case .focusPaneDown: return KeyChord("down", command: true, option: true)
        case .zoomPane: return KeyChord("enter", command: true, shift: true)
        case .equalizePanes: return KeyChord("=", command: true, option: true)
        case .closeSplit: return KeyChord("w", command: true, shift: true)

        case .inbox: return KeyChord("i", command: true)
        case .viewTerminal: return KeyChord("1", command: true, option: true)
        case .viewDiff: return KeyChord("2", command: true, option: true)
        case .viewPR: return KeyChord("3", command: true, option: true)
        case .viewActivity: return KeyChord("4", command: true, option: true)
        case .toggleSidebar: return KeyChord("s", command: true, control: true)
        case .togglePearlRail: return KeyChord("p", command: true, control: true)
        case .settings: return KeyChord(",", command: true)

        default:
            if let n = Self.focusSessionIndex(self) { return KeyChord(String(n + 1), command: true) }
            return nil
        }
    }

    /// A one-line "what this does", shown under the row in Settings ▸ Keyboard
    /// where the title alone is not enough.
    var note: String? {
        switch self {
        case .newShell: return "A shell session in the focused session's worktree, in a new tab."
        case .newTab: return "Another surface layout over the same fleet — tabs hold panes, not sessions."
        case .steerAll: return "Sends the steer bar's text to every working session."
        case .zoomPane: return "The focused pane fills the tab; again restores the layout."
        case .equalizePanes: return "Resets every divider in the focused tab to an even split."
        default: return nil
        }
    }
}

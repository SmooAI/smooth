import Foundation

/// How much the Activity tab has to show for a session's harness.
enum ActivityDepth: Equatable {
    /// `hooks` / `native` harnesses report their own state: the full event log.
    case full
    /// `scrape` harnesses are read off the screen: supervision events only.
    case thin
    /// A plain shell reports nothing.
    case none
}

/// Which center tabs the focused session can use (th-68d10a). Diff and PR
/// follow the WORKTREE — a shell in a worktree has a branch, a diff and maybe a
/// PR — and only Activity follows the session kind. Tabs are disabled, never
/// hidden: a tab that appears and vanishes on conditions you can't see is its
/// own friction.
struct CenterTabGate: Equatable {
    /// The session sits in a git repo: `git status` + the diff against the
    /// merge base mean something.
    var diff: Bool
    /// A repo AND a branch. With no PR yet the tab shows an empty state.
    var pr: Bool
    var activity: ActivityDepth

    static let nothingFocused = CenterTabGate(diff: false, pr: false, activity: .none)

    /// Pure: the session row, its handoff packet (once loaded), and the
    /// harness list.
    ///
    /// Before the packet arrives, a branch on the session row is taken as the
    /// sign of a repo, so a worktree session doesn't start with Diff greyed
    /// out and then flicker on. Once the packet is here, its `head` decides: no
    /// HEAD means `git rev-parse` failed, i.e. not a repo.
    static func of(session: Session?, packet: Handoff.Packet?, harnesses: [HarnessInfo]) -> CenterTabGate {
        guard let session else { return .nothingFocused }
        let branch = usableBranch(packet?.branch ?? session.branch)
        let inRepo: Bool
        if let packet {
            inRepo = !(packet.head ?? "").isEmpty
        } else {
            inRepo = branch != nil
        }
        return CenterTabGate(diff: inRepo, pr: inRepo && branch != nil, activity: activityDepth(kind: session.kind, harnesses: harnesses))
    }

    /// `git rev-parse --abbrev-ref HEAD` says `HEAD` when detached — no branch
    /// to hang a PR on.
    static func usableBranch(_ raw: String?) -> String? {
        guard let b = raw?.trimmingCharacters(in: .whitespacesAndNewlines), !b.isEmpty, b != "HEAD" else { return nil }
        return b
    }

    /// Activity is the one kind-gated tab, on the harness manifest's
    /// `state.source`. An unknown harness keeps the full view rather than
    /// hiding events it might well send.
    static func activityDepth(kind: String, harnesses: [HarnessInfo]) -> ActivityDepth {
        if kind == "shell" { return .none }
        switch harnesses.first(where: { $0.name == kind })?.stateSource {
        case "scrape": return .thin
        default: return .full
        }
    }

    func allows(_ tab: CenterTab) -> Bool {
        switch tab {
        case .terminal: true
        case .diff: diff
        case .pr: pr
        case .activity: activity != .none
        }
    }

    /// The tab to land on when the focused session can't use the current one.
    func resolve(_ tab: CenterTab) -> CenterTab { allows(tab) ? tab : .terminal }

    /// Tooltip for a disabled segment, so it never reads as broken.
    func whyNot(_ tab: CenterTab) -> String? {
        guard !allows(tab) else { return nil }
        switch tab {
        case .terminal: return nil
        case .diff: return "Not in a git repository"
        case .pr: return diff ? "No branch (detached HEAD)" : "Not in a git repository"
        case .activity: return "A shell reports no activity"
        }
    }
}

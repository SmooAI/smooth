import Foundation

/// The close-out decision, as a pure function of what the shell already knows
/// about a session (th-fe75ca).
///
/// Everything the sheet says — the toggles it offers, their defaults, and the
/// lines naming what is about to be destroyed — is computed here so it can be
/// tested apart from AppKit presentation, the way `PaneClose.decide` is.
///
/// What it deliberately does NOT compute: whether the branch is merged. The
/// engine reveals that at the only moment it matters, by refusing the close
/// with a reason shown verbatim; a flag cached here would be a second source
/// of truth that can disagree at the instant of the close.
enum SessionClose {
    /// What closing THIS session would do, in the words the sheet uses.
    struct Plan: Equatable {
        /// Sheet headline.
        var title: String
        /// Live sessions are killed by the engine before anything else — said
        /// plainly, before you confirm, never discovered afterwards.
        var isLive: Bool
        var liveWarning: String?
        /// The pearl toggle, or the line explaining there is none.
        var pearlId: String?
        var pearlLabel: String?
        var pearlDetail: String?
        var pearlNote: String?
        var defaultClosePearl: Bool
        /// The worktree toggle, or the line explaining the main checkout stays.
        var hasOwnWorktree: Bool
        var worktreeLabel: String?
        var branchLabel: String?
        var worktreeNote: String
        var defaultRemoveWorktree: Bool
        /// Uncommitted files as of the last handoff read — the thing most
        /// likely to be lost, and the usual reason the engine refuses.
        var dirtyCount: Int
        var dirtyLabel: String?
        /// The affirmative button.
        var confirmTitle: String
    }

    /// The main checkout is never removed (the engine refuses too); only a row
    /// that lives in its own worktree offers the toggle.
    ///
    /// A row with no project at all (a plain shell started in `$HOME`) does not
    /// qualify: there is nothing to compare its directory against, and the
    /// alternative is a sheet offering to delete the home directory.
    static func hasOwnWorktree(_ s: Session) -> Bool { !s.worktree.isEmpty && !s.project.isEmpty && s.worktree != s.project }

    static func decide(session s: Session, handoff h: Handoff?) -> Plan {
        let live = s.isLive
        let own = hasOwnWorktree(s)
        let dirty = h?.handoff?.dirty?.count ?? 0
        let branch = s.branch ?? h?.handoff?.branch
        let pearlTitle = h?.pearl?.title

        return Plan(
            title: "Close \(s.label)",
            isLive: live,
            liveWarning: live
                ? "This session is still running — closing kills it first. The agent stops mid-turn; its scrollback stays until you quit."
                : nil,
            pearlId: s.pearlId,
            pearlLabel: s.pearlId.map { "Close pearl \($0)" },
            pearlDetail: s.pearlId != nil ? pearlTitle : nil,
            pearlNote: s.pearlId == nil ? "No pearl on this session." : nil,
            defaultClosePearl: s.pearlId != nil,
            hasOwnWorktree: own,
            worktreeLabel: own ? "Remove worktree \(s.worktree)" : nil,
            branchLabel: own ? branch.map { "and delete branch \($0)" } : nil,
            worktreeNote: own
                ? "Only once the branch is merged and the worktree is clean; otherwise the engine refuses and touches nothing — you can force it after reading why."
                : "Main checkout — the worktree is kept.",
            defaultRemoveWorktree: own,
            dirtyCount: dirty,
            dirtyLabel: own && dirty > 0 ? "\(dirty) uncommitted file\(dirty == 1 ? "" : "s") right now — a plain close will be refused." : nil,
            confirmTitle: live ? "Kill and close" : "Close session"
        )
    }
}

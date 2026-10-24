import Foundation

/// Where a new shell opens (SMOODEV-4111). SmoothFlow starts like a
/// terminal: ⌘N and every split open a plain shell at once, and the only
/// question is the directory — answered here, in this order:
///
/// 1. a directory the user just picked (New Shell in Directory…);
/// 2. the focused pane's session, by id: the engine asks tmux where its
///    shell is RIGHT NOW (`cwd_of`), so a `cd` inside the pane carries over
///    and the session's launch worktree is only the engine's fallback;
/// 3. the last directory the user opened, if it still exists;
/// 4. `$HOME`.
enum ShellLaunch {
    static func request(source: Session?, picked: String? = nil, last: String?, home: String,
                        isDirectory: (String) -> Bool = ShellLaunch.isDirectory) -> NewSession {
        var n = NewSession(kind: "shell")
        if let picked, !picked.isEmpty {
            n.worktree = picked
        } else if let source {
            n.cwdOf = source.id
        } else if let last, !last.isEmpty, isDirectory(last) {
            n.worktree = last
        } else {
            n.worktree = home
        }
        return n
    }

    /// SMOODEV-4127: the `flow.prewarm` hint for where the next ⌘N would
    /// open — the same answer `request` gives, sent ahead of time.
    static func prewarm(source: Session?, last: String?, home: String,
                        isDirectory: (String) -> Bool = ShellLaunch.isDirectory) -> ClientFrame {
        let n = request(source: source, last: last, home: home, isDirectory: isDirectory)
        return .prewarm(worktree: n.worktree, cwdOf: n.cwdOf)
    }

    static func isDirectory(_ path: String) -> Bool {
        var dir: ObjCBool = false
        return FileManager.default.fileExists(atPath: path, isDirectory: &dir) && dir.boolValue
    }
}

/// The last directory the user opened a shell or session in, remembered
/// across launches so ⌘N with nothing focused lands somewhere useful.
enum LastDirectory {
    static let key = "lastShellDirectory"

    static func load(_ d: UserDefaults = .standard) -> String? { d.string(forKey: key) }

    /// Only absolute paths are worth remembering; anything else is ignored.
    static func remember(_ path: String?, _ d: UserDefaults = .standard) {
        guard let path, path.hasPrefix("/") else { return }
        d.set(path, forKey: key)
    }
}

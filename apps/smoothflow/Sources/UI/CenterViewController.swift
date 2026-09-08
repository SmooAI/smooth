import AppKit
import SwiftUI

enum CenterTab: Int { case terminal, diff, pr, activity }

/// Center column: tab strip (terminal / diff / PR), a splittable surface area,
/// and the steer bar. Surfaces are owned by `AppController` and merely hosted here.
@MainActor
final class CenterViewController: NSViewController, NSTextFieldDelegate {
    unowned let app: AppController

    private let tabs = NSSegmentedControl(labels: ["terminal", "diff", "PR", "activity"], trackingMode: .selectOne, target: nil, action: nil)
    private let pathLabel = NSTextField(labelWithString: "")
    private let split = NSSplitView()
    private var panes: [SessionPane] = []
    private var activePane = 0
    private let diffView = DiffView()
    private var prHost: NSHostingView<PRView>?
    private var activityHost: NSHostingView<ActivityView>?
    private let steerField = NSTextField()
    private let steerHint = NSTextField(labelWithString: "⌘↵ send · ⌘⇧↵ send to all working")
    private let content = NSView()

    init(app: AppController) {
        self.app = app
        super.init(nibName: nil, bundle: nil)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { nil }

    override func loadView() {
        let root = NSView()
        root.translatesAutoresizingMaskIntoConstraints = false

        tabs.selectedSegment = 0
        tabs.target = self
        tabs.action = #selector(tabChanged)
        pathLabel.font = .monospacedSystemFont(ofSize: 11, weight: .regular)
        pathLabel.textColor = Theme.muted
        pathLabel.lineBreakMode = .byTruncatingMiddle
        let top = NSStackView(views: [tabs, pathLabel])
        top.orientation = .horizontal
        top.spacing = 12
        top.edgeInsets = NSEdgeInsets(top: 6, left: 10, bottom: 6, right: 10)

        split.isVertical = true
        split.dividerStyle = .thin
        content.translatesAutoresizingMaskIntoConstraints = false
        addPane()
        show(split)

        steerField.placeholderString = "Steer the focused session…"
        steerField.delegate = self
        steerField.font = .systemFont(ofSize: 13)
        steerHint.font = .systemFont(ofSize: 10)
        steerHint.textColor = Theme.faint
        let steer = NSStackView(views: [steerField, steerHint])
        steer.orientation = .horizontal
        steer.edgeInsets = NSEdgeInsets(top: 6, left: 10, bottom: 8, right: 10)
        steerField.setContentHuggingPriority(.defaultLow, for: .horizontal)

        let column = NSStackView(views: [top, content, steer])
        column.orientation = .vertical
        column.spacing = 0
        column.translatesAutoresizingMaskIntoConstraints = false
        root.addSubview(column)
        NSLayoutConstraint.activate([
            column.leadingAnchor.constraint(equalTo: root.leadingAnchor),
            column.trailingAnchor.constraint(equalTo: root.trailingAnchor),
            column.topAnchor.constraint(equalTo: root.topAnchor),
            column.bottomAnchor.constraint(equalTo: root.bottomAnchor),
            content.widthAnchor.constraint(equalTo: column.widthAnchor),
        ])
        content.setContentHuggingPriority(.defaultLow, for: .vertical)
        view = root
    }

    private func show(_ v: NSView) {
        content.subviews.forEach { $0.removeFromSuperview() }
        v.translatesAutoresizingMaskIntoConstraints = false
        content.addSubview(v)
        NSLayoutConstraint.activate([
            v.leadingAnchor.constraint(equalTo: content.leadingAnchor), v.trailingAnchor.constraint(equalTo: content.trailingAnchor),
            v.topAnchor.constraint(equalTo: content.topAnchor), v.bottomAnchor.constraint(equalTo: content.bottomAnchor),
        ])
    }

    // MARK: tabs

    var tab: CenterTab = .terminal {
        didSet {
            tabs.selectedSegment = tab.rawValue
            switch tab {
            case .terminal: show(split)
            case .diff:
                show(diffView)
                diffView.load(worktree: app.store.focused?.worktree)
            case .pr:
                let host = NSHostingView(rootView: PRView(app: app, store: app.store))
                prHost = host
                show(host)
            case .activity:
                let host = NSHostingView(rootView: ActivityView(store: app.store))
                activityHost = host
                show(host)
            }
        }
    }

    @objc private func tabChanged() { tab = CenterTab(rawValue: tabs.selectedSegment) ?? .terminal }

    // MARK: panes

    func addPane() {
        let pane = SessionPane()
        pane.onActivate = { [weak self, weak pane] in
            guard let self, let pane, let i = self.panes.firstIndex(where: { $0 === pane }) else { return }
            self.activePane = i
            if let id = pane.sessionId { self.app.store.focusedId = id }
        }
        panes.append(pane)
        split.addArrangedSubview(pane)
        split.adjustSubviews()
    }

    func splitActive() {
        addPane()
        activePane = panes.count - 1
        if let id = app.store.focusedId { showSession(id) }
    }

    func closeActivePane() {
        guard panes.count > 1 else { return }
        let pane = panes.remove(at: activePane)
        pane.removeFromSuperview()
        activePane = min(activePane, panes.count - 1)
        split.adjustSubviews()
    }

    /// The focused session goes into the active pane.
    func showSession(_ id: String) {
        guard let s = app.store.sessions[id] else { return }
        let surface = app.surface(for: id)
        panes[activePane].host(surface, id: id)
        pathLabel.stringValue = "\(s.worktree) · \(s.argv.joined(separator: " "))"
        if tab == .diff { diffView.load(worktree: s.worktree) }
        view.window?.makeFirstResponder(surface)
    }

    func refreshHeaders() {
        for p in panes { if let id = p.sessionId, let s = app.store.sessions[id] { p.setHeader(s) } }
        // argv changes under a session (`claude --resume …` after a kill/resume).
        if let s = app.store.focused { pathLabel.stringValue = "\(s.worktree) · \(s.argv.joined(separator: " "))" }
    }

    func focusSteer() { view.window?.makeFirstResponder(steerField) }

    // MARK: steer

    func control(_ control: NSControl, textView: NSTextView, doCommandBy selector: Selector) -> Bool {
        guard selector == #selector(NSResponder.insertNewline(_:)) else { return false }
        let all = NSApp.currentEvent?.modifierFlags.contains(.shift) == true
        let text = steerField.stringValue.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty else { return true }
        app.steer(text, toAll: all)
        steerField.stringValue = ""
        return true
    }

    func steerAll() {
        let text = steerField.stringValue.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty else { focusSteer(); return }
        app.steer(text, toAll: true)
        steerField.stringValue = ""
    }
}

/// One split pane: a header line + the hosted terminal surface.
@MainActor
final class SessionPane: NSView {
    private(set) var sessionId: String?
    var onActivate: (() -> Void)?
    private let header = NSTextField(labelWithString: "")
    private let dot = NSView()
    private var hosted: NSView?

    init() {
        super.init(frame: .zero)
        header.font = .systemFont(ofSize: 11)
        header.textColor = Theme.muted
        dot.wantsLayer = true
        dot.layer?.cornerRadius = 4
        dot.translatesAutoresizingMaskIntoConstraints = false
        header.translatesAutoresizingMaskIntoConstraints = false
        addSubview(dot)
        addSubview(header)
        NSLayoutConstraint.activate([
            dot.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 10), dot.topAnchor.constraint(equalTo: topAnchor, constant: 8),
            dot.widthAnchor.constraint(equalToConstant: 8), dot.heightAnchor.constraint(equalToConstant: 8),
            header.leadingAnchor.constraint(equalTo: dot.trailingAnchor, constant: 6), header.centerYAnchor.constraint(equalTo: dot.centerYAnchor),
            header.trailingAnchor.constraint(lessThanOrEqualTo: trailingAnchor, constant: -10),
        ])
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { nil }

    func host(_ view: TerminalSurfaceView, id: String) {
        sessionId = id
        hosted?.removeFromSuperview()
        hosted = view
        view.translatesAutoresizingMaskIntoConstraints = false
        addSubview(view)
        NSLayoutConstraint.activate([
            view.leadingAnchor.constraint(equalTo: leadingAnchor), view.trailingAnchor.constraint(equalTo: trailingAnchor),
            view.topAnchor.constraint(equalTo: topAnchor, constant: 24), view.bottomAnchor.constraint(equalTo: bottomAnchor),
        ])
        let previous = view.onFocus
        view.onFocus = { [weak self] focused in
            previous?(focused)
            if focused { self?.onActivate?() }
        }
    }

    func setHeader(_ s: Session) {
        header.stringValue = "\(s.pearlId ?? s.title) · \(s.kind == "claude" ? "Claude Code" : s.kind) · \(Theme.stateLabel(s))"
        dot.layer?.backgroundColor = NSColor(Theme.dot(for: s)).cgColor
    }
}

/// `git diff` of the focused worktree. A view over git, not shell state.
@MainActor
final class DiffView: NSScrollView {
    private let text = NSTextView()

    init() {
        super.init(frame: .zero)
        documentView = text
        hasVerticalScroller = true
        text.isEditable = false
        text.font = .monospacedSystemFont(ofSize: 12, weight: .regular)
        text.autoresizingMask = [.width]
        text.isVerticallyResizable = true
        text.textContainer?.widthTracksTextView = true
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { nil }

    func load(worktree: String?) {
        guard let worktree, !worktree.isEmpty else { text.string = "No worktree."; return }
        text.string = "Loading git diff for \(worktree)…"
        Task.detached {
            let out = Shell.run("/usr/bin/git", ["-C", worktree, "--no-pager", "diff", "--stat", "-p", "HEAD"], cwd: nil)
            let status = Shell.run("/usr/bin/git", ["-C", worktree, "status", "--short"], cwd: nil)
            let s = "# git status --short\n\(status)\n# git diff HEAD\n\(out.isEmpty ? "(clean)" : out)"
            await MainActor.run { self.text.string = s }
        }
    }
}

struct PRView: View {
    @ObservedObject var app: AppController
    @ObservedObject var store: FlowStore

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            if let s = store.focused {
                let pr = app.handoffs[s.id]?.pr
                if let pr, let n = pr.number {
                    Text("PR #\(n)").font(.title2.bold())
                    if let ci = pr.ci { Text("CI: \(ci)").font(.body) }
                    if let u = pr.url, let url = URL(string: u) {
                        Link(u, destination: url).font(.body.monospaced())
                        Button("Merge (opens PR)") { app.merge(s) }
                    }
                } else {
                    Text("PR · none yet").font(.title2.bold())
                    Text("The engine reports a PR in the handoff packet once `th` sees one on the branch \(s.branch ?? "").").font(.caption).foregroundStyle(Color(Theme.muted))
                }
            } else {
                Text("No session focused")
            }
            Spacer()
        }
        .padding(20)
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .task(id: store.focusedId) { if let id = store.focusedId { await app.loadHandoff(for: id) } }
    }
}

/// The focused session's `flow.event` lines, newest at the bottom. Quiet by
/// design: time and kind recede, the text carries it, and amber appears only
/// on the events that mean Big Smooth needs you.
struct ActivityView: View {
    @ObservedObject var store: FlowStore

    var body: some View {
        ScrollViewReader { proxy in
            ScrollView {
                if let id = store.focusedId {
                    let events = store.events[id] ?? []
                    if events.isEmpty {
                        Text("No activity yet · the engine sends `flow.event` as hooks and supervision fire")
                            .font(.caption).foregroundStyle(Color(Theme.muted)).padding(20)
                    } else {
                        LazyVStack(alignment: .leading, spacing: 4) {
                            ForEach(events) { e in row(e).id(e.id) }
                        }
                        .padding(14)
                        .onChange(of: events.count) { _, _ in if let last = events.last { proxy.scrollTo(last.id, anchor: .bottom) } }
                        .onAppear { if let last = events.last { proxy.scrollTo(last.id, anchor: .bottom) } }
                    }
                } else {
                    Text("No session focused").foregroundStyle(Color(Theme.muted)).padding(20)
                }
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
    }

    private func row(_ e: FlowEvent) -> some View {
        HStack(alignment: .top, spacing: 8) {
            Text(Self.clock(e.at)).font(.caption.monospaced()).foregroundStyle(Color(Theme.faint)).frame(width: 58, alignment: .trailing)
            Text(e.kind).font(.caption.monospaced()).foregroundStyle(e.needsYou ? Color(Theme.amber) : Color(Theme.muted)).frame(width: 96, alignment: .leading)
            Text(e.text).font(.caption).textSelection(.enabled).lineLimit(4)
        }
    }

    /// `HH:mm:ss` local, or the raw text when it is not a timestamp.
    static func clock(_ iso: String) -> String {
        guard let d = ISO8601DateFormatter.flexible.date(from: iso) else { return String(iso.prefix(8)) }
        return clockFormatter.string(from: d)
    }

    private static let clockFormatter: DateFormatter = { let f = DateFormatter(); f.dateFormat = "HH:mm:ss"; return f }()
}

enum Shell {
    /// Blocking helper for short local commands (git, th). Not for agents — those are the engine's.
    static func run(_ exe: String, _ args: [String], cwd: String?) -> String {
        let p = Process()
        p.executableURL = URL(fileURLWithPath: exe)
        p.arguments = args
        if let cwd, !cwd.isEmpty { p.currentDirectoryURL = URL(fileURLWithPath: cwd) }
        var env = ProcessInfo.processInfo.environment
        env["PATH"] = "\(FileManager.default.homeDirectoryForCurrentUser.path)/.cargo/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin"
        p.environment = env
        let pipe = Pipe()
        p.standardOutput = pipe
        p.standardError = pipe
        do { try p.run() } catch { return "failed to run \(exe): \(error.localizedDescription)" }
        let data = pipe.fileHandleForReading.readDataToEndOfFile()
        p.waitUntilExit()
        return String(data: data, encoding: .utf8) ?? ""
    }

    static func which(_ name: String) -> String? {
        let home = FileManager.default.homeDirectoryForCurrentUser.path
        return ["\(home)/.cargo/bin/\(name)", "/opt/homebrew/bin/\(name)", "/usr/local/bin/\(name)", "/usr/bin/\(name)"]
            .first { FileManager.default.isExecutableFile(atPath: $0) }
    }
}

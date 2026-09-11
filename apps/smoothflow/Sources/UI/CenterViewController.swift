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
    /// Surface tabs (⌘T) over a pane tree (⌘D and friends). Never empty.
    private let tabBar = SurfaceTabBar()
    private let treeView = PaneTreeView()
    private let surfaceArea = NSStackView()
    private var surfaceTabs: [SurfaceTab] = []
    private var activeTabIndex = 0
    private var nextTabId = 1
    private let diffView = DiffView()
    private var prHost: NSHostingView<PRView>?
    private var activityHost: NSHostingView<ActivityView>?
    private let steerField = NSTextField()
    private let steerHint = NSTextField(labelWithString: "")
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
        tabs.setAccessibilityIdentifier("center.tabs")
        pathLabel.setAccessibilityIdentifier("center.path")
        steerField.setAccessibilityIdentifier("steer.field")
        tabs.target = self
        tabs.action = #selector(tabChanged)
        pathLabel.font = Theme.monoNSFont(size: 11)
        pathLabel.textColor = Theme.muted
        pathLabel.lineBreakMode = .byTruncatingMiddle
        let top = NSStackView(views: [tabs, pathLabel])
        top.orientation = .horizontal
        top.spacing = 12
        top.edgeInsets = NSEdgeInsets(top: 6, left: 10, bottom: 6, right: 10)

        content.translatesAutoresizingMaskIntoConstraints = false
        buildSurfaceArea()
        show(surfaceArea)

        steerField.placeholderString = "Steer the focused session…"
        steerField.delegate = self
        steerField.font = .systemFont(ofSize: 13)
        // The hint names whatever the keymap currently says, or it becomes the
        // one place in the app still advertising the old ⌘⇧↩.
        let focusedChord = app.keymap.chord(for: .steerFocused)?.display ?? "↵"
        let allChord = app.keymap.chord(for: .steerAll)?.display
        steerHint.stringValue = "\(focusedChord) send" + (allChord.map { " · \($0) send to all working" } ?? "")
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
            case .terminal: show(surfaceArea)
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

    // MARK: tabs + panes

    private func buildSurfaceArea() {
        surfaceArea.orientation = .vertical
        surfaceArea.spacing = 0
        surfaceArea.alignment = .leading
        surfaceArea.addArrangedSubview(tabBar)
        surfaceArea.addArrangedSubview(treeView)
        treeView.translatesAutoresizingMaskIntoConstraints = false
        NSLayoutConstraint.activate([
            tabBar.widthAnchor.constraint(equalTo: surfaceArea.widthAnchor),
            treeView.widthAnchor.constraint(equalTo: surfaceArea.widthAnchor),
        ])
        tabBar.onSelect = { [weak self] i in self?.selectTab(i) }
        tabBar.onClose = { [weak self] i in self?.closeTab(at: i) }
        tabBar.onNew = { [weak self] in self?.newTab() }
        treeView.onPaneActivated = { [weak self] id in self?.paneActivated(id) }
        treeView.onFractionChanged = { [weak self] f, a, b in
            guard let self, var t = self.activeTab else { return }
            t.root = t.root.settingFraction(f, between: a, and: b)
            self.replaceActiveTab(t, relayout: false)
        }
        surfaceTabs = [SurfaceTab(id: nextTabId, pane: PaneID.next())]
        nextTabId += 1
        renderTabs()
    }

    private var activeTab: SurfaceTab? {
        surfaceTabs.indices.contains(activeTabIndex) ? surfaceTabs[activeTabIndex] : nil
    }

    private func replaceActiveTab(_ t: SurfaceTab, relayout: Bool = true, redividing: Bool = false) {
        guard surfaceTabs.indices.contains(activeTabIndex) else { return }
        surfaceTabs[activeTabIndex] = t
        if relayout { renderTabs(redividing: redividing) }
    }

    /// Lay the active tab's tree out, re-host every pane's surface, refresh the
    /// strip. The one place the model reaches the screen.
    private func renderTabs(redividing: Bool = false) {
        guard let t = activeTab else { return }
        treeView.apply(root: t.root, focused: t.focused, zoomed: t.zoomed, redividing: redividing)
        for id in t.panes {
            let pane = treeView.pane(id)
            if let sid = t.sessions[id], app.store.sessions[sid] != nil {
                pane.host(app.surface(for: sid), id: sid)
            } else {
                pane.showEmpty()
            }
        }
        tabBar.update(titles: surfaceTabs.map { tab in tab.title { self.app.store.sessions[$0]?.pearlId ?? self.app.store.sessions[$0]?.title } }, active: activeTabIndex)
        refreshHeaders()
        if let sid = t.sessions[t.focused], app.store.focusedId != sid { app.store.focusedId = sid }
    }

    /// A pane took focus (click, or the surface became first responder): it is
    /// now the focused pane, and its session is the focused session.
    private func paneActivated(_ id: PaneID) {
        guard var t = activeTab, t.focused != id || t.zoomed != nil else { return }
        t.focused = id
        replaceActiveTab(t, relayout: false)
        treeView.apply(root: t.root, focused: t.focused, zoomed: t.zoomed)
        if let sid = t.sessions[id] { app.store.focusedId = sid }
    }

    // MARK: tab actions

    func newTab() {
        var t = SurfaceTab(id: nextTabId, pane: PaneID.next())
        nextTabId += 1
        if let sid = app.store.focusedId { t.sessions[t.focused] = sid }
        surfaceTabs.append(t)
        activeTabIndex = surfaceTabs.count - 1
        renderTabs()
    }

    /// A shell session (`kind=shell`) in the focused session's worktree, in a
    /// new tab — the "give me a prompt next to the agent" move. The tab shows
    /// it as soon as the engine announces the session.
    func newShellHere() {
        guard let s = app.store.focused else { return }
        newTab()
        pendingShellTab = activeTab?.id
        app.newSession(NewSession(kind: "shell", worktree: s.worktree, project: s.projectName, title: "shell · \(s.projectName)"))
    }

    private var pendingShellTab: Int?

    /// A brand-new session claims the tab that asked for it (`newShellHere`),
    /// else nothing — a session appearing must never steal a pane you are in.
    func adopt(newSessionId id: String) {
        guard let tabId = pendingShellTab, let i = surfaceTabs.firstIndex(where: { $0.id == tabId }) else { return }
        pendingShellTab = nil
        surfaceTabs[i].sessions[surfaceTabs[i].focused] = id
        renderTabs()
    }

    func closeTab(at index: Int? = nil) {
        let i = index ?? activeTabIndex
        guard surfaceTabs.indices.contains(i), surfaceTabs.count > 1 else { return }
        surfaceTabs.remove(at: i)
        activeTabIndex = min(activeTabIndex >= i ? max(activeTabIndex - 1, 0) : activeTabIndex, surfaceTabs.count - 1)
        renderTabs()
    }

    func selectTab(_ i: Int) {
        guard surfaceTabs.indices.contains(i), i != activeTabIndex else { return }
        activeTabIndex = i
        renderTabs()
    }

    func cycleTab(by delta: Int) {
        guard surfaceTabs.count > 1 else { return }
        let n = surfaceTabs.count
        selectTab(((activeTabIndex + delta) % n + n) % n)
    }

    // MARK: split actions

    func split(_ direction: SplitDirection) {
        guard var t = activeTab else { return }
        _ = t.split(direction)
        replaceActiveTab(t)
        focusActiveSurface()
    }

    /// ⌘W. Terminal semantics: the focused pane goes, and the container
    /// collapses when it empties — last pane closes the tab, last tab closes
    /// the window. A pane holding a live session asks first, and the alert is
    /// where the two honest answers live: close the view (the session keeps
    /// running in the fleet) or end the session (it does not). See
    /// `PaneClose.decide`.
    func closeFocusedPane() {
        guard let t = activeTab else { return }
        let scope: PaneCloseScope = t.panes.count > 1 ? .pane : (surfaceTabs.count > 1 ? .tab : .window)
        let session = t.sessions[t.focused].flatMap { app.store.sessions[$0] }
        let decision = PaneClose.decide(session: session,
                                        harnessLabel: session.map { app.store.displayName(forKind: $0.kind) } ?? "",
                                        scope: scope,
                                        confirmEnabled: PaneCloseSettings.confirm())
        guard let prompt = decision.prompt, let session, let window = view.window else {
            return performClose(scope: scope, kill: nil)
        }
        let alert = NSAlert()
        alert.alertStyle = .warning
        alert.messageText = prompt.title
        alert.informativeText = prompt.message
        alert.showsSuppressionButton = true
        alert.suppressionButton?.title = "Don’t ask again"
        alert.suppressionButton?.setAccessibilityIdentifier("pane.close.suppress")
        alert.addButton(withTitle: prompt.closeTitle)
        if let kill = prompt.killTitle { alert.addButton(withTitle: kill) }
        alert.addButton(withTitle: "Cancel")
        alert.buttons[0].setAccessibilityIdentifier("pane.close.close")
        alert.buttons[0].keyEquivalent = ""
        if prompt.killTitle != nil {
            alert.buttons[1].setAccessibilityIdentifier("pane.close.kill")
            alert.buttons[1].hasDestructiveAction = true
            alert.buttons[1].keyEquivalent = ""
        }
        // Cancel is the DEFAULT: a stray Return over this sheet must never kill
        // an agent. That is why the buttons are re-keyed rather than ordered
        // Cancel-first, which would put it on the wrong side of the sheet.
        let cancel = alert.buttons[alert.buttons.count - 1]
        cancel.setAccessibilityIdentifier("pane.close.cancel")
        cancel.keyEquivalent = "\r"
        alert.beginSheetModal(for: window) { [weak self] response in
            guard let self else { return }
            if alert.suppressionButton?.state == .on { PaneCloseSettings.setConfirm(false) }
            switch response {
            case .alertFirstButtonReturn: self.performClose(scope: scope, kill: nil)
            case .alertSecondButtonReturn where prompt.killTitle != nil: self.performClose(scope: scope, kill: session)
            default: break
            }
        }
    }

    /// Remove the focused pane, taking the tab and then the window with it when
    /// they empty. `kill` ends that pane's session on the way out.
    private func performClose(scope: PaneCloseScope, kill: Session?) {
        if let kill { app.kill(kill, resume: false) }
        switch scope {
        case .pane:
            guard var t = activeTab else { return }
            _ = t.closeFocused()
            replaceActiveTab(t)
        case .tab:
            closeTab()
        case .window:
            view.window?.performClose(nil)
        }
    }

    /// Remove the focused pane with no questions — the tab-bar × and the
    /// internal callers. ⌘W goes through `closeFocusedPane`.
    func closeActivePane() {
        guard var t = activeTab else { return }
        if t.closeFocused() {
            replaceActiveTab(t)
        } else {
            closeTab()
        }
    }

    func focusPane(_ direction: SplitDirection) {
        guard var t = activeTab else { return }
        let before = t.focused
        t.focus(direction, in: treeView.bounds)
        guard t.focused != before else { return }
        replaceActiveTab(t, relayout: false)
        treeView.apply(root: t.root, focused: t.focused, zoomed: t.zoomed)
        if let sid = t.sessions[t.focused] { app.store.focusedId = sid }
        focusActiveSurface()
    }

    func toggleZoom() {
        guard var t = activeTab else { return }
        t.toggleZoom()
        replaceActiveTab(t)
        focusActiveSurface()
    }

    func equalizePanes() {
        guard var t = activeTab else { return }
        t.equalize()
        replaceActiveTab(t, redividing: true)
    }

    private func focusActiveSurface() {
        guard let t = activeTab, let sid = t.sessions[t.focused], let surface = app.surfaceIfLoaded(sid) else { return }
        view.window?.makeFirstResponder(surface)
    }

    /// The focused session goes into the focused pane of the active tab.
    func showSession(_ id: String) {
        guard let s = app.store.sessions[id], var t = activeTab else { return }
        // Already on screen in this tab? Move the focus there instead of
        // stacking the same session into a second pane.
        if let existing = t.panes.first(where: { t.sessions[$0] == id }) {
            guard t.focused != existing else { return }
            t.focused = existing
        } else {
            guard t.sessions[t.focused] != id else { return }
            t.sessions[t.focused] = id
        }
        replaceActiveTab(t)
        pathLabel.stringValue = "\(s.worktree) · \(s.argv.joined(separator: " "))"
        if tab == .diff { diffView.load(worktree: s.worktree) }
        focusActiveSurface()
    }

    func refreshHeaders() {
        guard let t = activeTab else { return }
        for id in t.panes {
            guard let sid = t.sessions[id], let s = app.store.sessions[sid] else { continue }
            treeView.pane(id).setHeader(s, kindLabel: app.store.displayName(forKind: s.kind))
        }
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
        wantsLayer = true
        layer?.borderWidth = 1
        layer?.borderColor = NSColor.clear.cgColor
        header.font = .systemFont(ofSize: 11)
        header.setAccessibilityIdentifier("pane.header")
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
        // Re-hosting what is already here would chain another `onFocus`
        // wrapper onto the surface every time the tab is laid out.
        guard sessionId != id || hosted !== view else { return }
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

    func setHeader(_ s: Session, kindLabel: String) {
        header.stringValue = "\(s.pearlId ?? s.title) · \(kindLabel) · \(Theme.stateLabel(s))"
        dot.layer?.backgroundColor = NSColor(Theme.dot(for: s)).cgColor
    }

    /// A pane with nothing in it yet — a fresh split before you pick a session.
    func showEmpty() {
        guard sessionId != nil || hosted == nil else { return }
        sessionId = nil
        hosted?.removeFromSuperview()
        hosted = nil
        header.stringValue = "empty · pick a session in the sidebar"
        dot.layer?.backgroundColor = Theme.faint.cgColor
    }

    /// Which pane the keyboard is in. A one-pixel teal edge, and only when the
    /// tab is actually split — a lone pane needs no telling.
    func setFocused(_ focused: Bool) {
        layer?.borderColor = (focused ? Theme.teal.withAlphaComponent(0.7) : .clear).cgColor
        setAccessibilityIdentifier(focused ? "pane.focused" : "pane")
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
        text.font = Theme.monoNSFont(size: 12)
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
                        Link(u, destination: url).font(Theme.mono(.body))
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
            Text(Self.clock(e.at)).font(Theme.mono(.caption)).foregroundStyle(Color(Theme.faint)).frame(width: 58, alignment: .trailing)
            Text(e.kind).font(Theme.mono(.caption)).foregroundStyle(e.needsYou ? Color(Theme.amber) : Color(Theme.muted)).frame(width: 96, alignment: .leading)
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

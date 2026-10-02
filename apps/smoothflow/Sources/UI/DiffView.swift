import AppKit

/// The Diff tab (th-26f5b9): a native review viewer over the engine's
/// structured diffs. File tree with status badges, counts and viewed marks;
/// unified or side-by-side; Catppuccin Mocha syntax and word-level highlight
/// straight from the engine's spans; noise collapsed; a base picker (last
/// turn / uncommitted / vs main); per-hunk Revert and Stage; comments on a
/// line or range, sent to the agent as one review.
///
/// The main table is virtualized (an `NSTableView` over computed `DiffRow`s,
/// lines drawn, not laid out), so a diff of thousands of lines costs only
/// what is on screen. Keys come from the keymap's view-scoped diff actions.
@MainActor
final class DiffView: NSView, NSTableViewDataSource, NSTableViewDelegate {
    unowned let app: AppController

    // MARK: state

    private(set) var sessionId: String?
    private var sessionKind = "shell"
    private(set) var base: DiffBase = .uncommitted
    /// The base the user picked, per session (else the kind's default).
    private var baseChosen: [String: DiffBase] = [:]
    private(set) var diff: DiffPayload?
    private(set) var split = UserDefaults.standard.bool(forKey: "diffSplit")
    /// Per session: viewed keys (`DiffRules.viewedKey`) — in memory.
    private var viewed: [String: Set<String>] = [:]
    /// Per session: the user's expand/collapse by path, over the rules.
    private var toggles: [String: [String: Bool]] = [:]
    private var comments: [String: [DiffComment]] = [:]
    private(set) var rows: [DiffRow] = []
    private var treeRows: [DiffRules.TreeRow] = []
    private var styled: [String: NSAttributedString] = [:]
    private var cursor: Int?
    private var anchor: Int?
    private var refreshScheduled = false

    // MARK: views

    private let baseControl = NSSegmentedControl(labels: ["Last turn", "Uncommitted", "vs main"], trackingMode: .selectOne, target: nil, action: nil)
    private let modeControl = NSSegmentedControl(labels: ["Unified", "Split"], trackingMode: .selectOne, target: nil, action: nil)
    private let summary = NSTextField(labelWithString: "")
    private let status = NSTextField(labelWithString: "")
    private let refreshButton = NSButton(title: "Refresh", target: nil, action: nil)
    private let reviewButton = NSButton(title: "Send review", target: nil, action: nil)
    private let treeTable = NSTableView()
    private let mainTable = DiffTableView()
    private let font = Theme.monoNSFont(size: 12)
    private lazy var charWidth: CGFloat = ("M" as NSString).size(withAttributes: [.font: font]).width

    init(app: AppController) {
        self.app = app
        super.init(frame: .zero)
        appearance = NSAppearance(named: .darkAqua)
        wantsLayer = true
        layer?.backgroundColor = DiffPalette.base.cgColor
        build()
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { nil }

    private func build() {
        baseControl.target = self
        baseControl.action = #selector(baseChanged)
        baseControl.setAccessibilityIdentifier("center.diff.base")
        baseControl.controlSize = .small
        modeControl.target = self
        modeControl.action = #selector(modeChanged)
        modeControl.selectedSegment = split ? 1 : 0
        modeControl.setAccessibilityIdentifier("center.diff.mode")
        modeControl.controlSize = .small
        summary.font = Theme.monoNSFont(size: 11)
        summary.textColor = DiffPalette.subtext
        summary.setAccessibilityIdentifier("center.diff.summary")
        summary.lineBreakMode = .byTruncatingTail
        status.font = .systemFont(ofSize: 11)
        status.textColor = DiffPalette.peach
        status.lineBreakMode = .byTruncatingTail
        status.setAccessibilityIdentifier("center.diff.status")
        refreshButton.bezelStyle = .rounded
        refreshButton.controlSize = .small
        refreshButton.target = self
        refreshButton.action = #selector(refreshClicked)
        refreshButton.setAccessibilityIdentifier("center.diff.refresh")
        reviewButton.bezelStyle = .rounded
        reviewButton.controlSize = .small
        reviewButton.target = self
        reviewButton.action = #selector(sendReview)
        reviewButton.setAccessibilityIdentifier("center.diff.review")
        reviewButton.isHidden = true
        summary.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        status.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)

        let bar = NSStackView(views: [baseControl, modeControl, summary, status, NSView(), reviewButton, refreshButton])
        bar.orientation = .horizontal
        bar.spacing = 10
        bar.edgeInsets = NSEdgeInsets(top: 6, left: 10, bottom: 6, right: 10)

        // File tree.
        let treeCol = NSTableColumn(identifier: .init("tree"))
        treeTable.addTableColumn(treeCol)
        treeTable.headerView = nil
        treeTable.backgroundColor = DiffPalette.mantle
        treeTable.rowHeight = 22
        treeTable.intercellSpacing = .zero
        treeTable.dataSource = self
        treeTable.delegate = self
        treeTable.target = self
        treeTable.action = #selector(treeClicked)
        treeTable.setAccessibilityIdentifier("center.diff.tree")
        let treeScroll = NSScrollView()
        treeScroll.documentView = treeTable
        treeScroll.hasVerticalScroller = true
        treeScroll.drawsBackground = false

        // Main table.
        let mainCol = NSTableColumn(identifier: .init("main"))
        mainCol.resizingMask = []
        mainTable.addTableColumn(mainCol)
        mainTable.headerView = nil
        mainTable.backgroundColor = DiffPalette.base
        mainTable.intercellSpacing = .zero
        mainTable.selectionHighlightStyle = .none
        mainTable.columnAutoresizingStyle = .noColumnAutoresizing
        mainTable.dataSource = self
        mainTable.delegate = self
        mainTable.owner = self
        mainTable.setAccessibilityIdentifier("center.diff.table")
        let mainScroll = NSScrollView()
        mainScroll.documentView = mainTable
        mainScroll.hasVerticalScroller = true
        mainScroll.hasHorizontalScroller = true
        mainScroll.drawsBackground = false
        mainScroll.postsFrameChangedNotifications = true
        NotificationCenter.default.addObserver(self, selector: #selector(resized), name: NSView.frameDidChangeNotification, object: mainScroll)

        let splitView = NSSplitView()
        splitView.isVertical = true
        splitView.dividerStyle = .thin
        splitView.addArrangedSubview(treeScroll)
        splitView.addArrangedSubview(mainScroll)
        treeScroll.widthAnchor.constraint(greaterThanOrEqualToConstant: 160).isActive = true
        let treeWidth = treeScroll.widthAnchor.constraint(equalToConstant: 240)
        treeWidth.priority = .defaultLow
        treeWidth.isActive = true
        splitView.setHoldingPriority(.defaultHigh, forSubviewAt: 0)

        let column = NSStackView(views: [bar, splitView])
        column.orientation = .vertical
        column.spacing = 0
        column.translatesAutoresizingMaskIntoConstraints = false
        addSubview(column)
        NSLayoutConstraint.activate([
            column.leadingAnchor.constraint(equalTo: leadingAnchor), column.trailingAnchor.constraint(equalTo: trailingAnchor),
            column.topAnchor.constraint(equalTo: topAnchor), column.bottomAnchor.constraint(equalTo: bottomAnchor),
            bar.widthAnchor.constraint(equalTo: column.widthAnchor), splitView.widthAnchor.constraint(equalTo: column.widthAnchor),
        ])
        syncBaseControl()
    }

    // MARK: session + loading

    func focusContent() { window?.makeFirstResponder(mainTable) }

    /// Show `s`'s diff: its chosen base, or Last turn for an agent and
    /// Uncommitted for a shell (spec §14).
    func show(session s: Session?) {
        guard let s else {
            sessionId = nil
            diff = nil
            summary.stringValue = "No session focused"
            rebuild()
            return
        }
        if s.id != sessionId {
            sessionId = s.id
            sessionKind = s.kind
            base = baseChosen[s.id] ?? DiffRules.defaultBase(kind: s.kind)
            diff = nil
            styled = [:]
            cursor = nil
            anchor = nil
            rebuild()
        }
        syncBaseControl()
        request()
    }

    private func request(path: String? = nil) {
        guard let id = sessionId else { return }
        if path == nil { status.stringValue = "Loading…" }
        app.sendDiff(.diff(id: id, base: base, path: path), sessionId: id)
    }

    /// `flow.diff` arrived.
    func receive(sessionId id: String, base b: DiffBase, path: String?, _ payload: DiffPayload) {
        guard id == sessionId, b == base else { return }
        if let path {
            // One file expanded: splice it in.
            guard var d = diff, let i = d.files.firstIndex(where: { $0.path == path }), let f = payload.files.first else { return }
            d.files[i] = f
            diff = d
            styled = styled.filter { !$0.key.hasPrefix("\(i)/") }
        } else {
            diff = payload
            styled = [:]
        }
        status.stringValue = ""
        if b == .branch {
            baseControl.setLabel(DiffRules.baseLabel(.branch, branchRef: DiffRules.branchRef(fromLabel: payload.from.label)), forSegment: 2)
        }
        updateSummary()
        rebuild()
    }

    /// A hunk action or review went through.
    func actionDone(_ r: DiffResult) {
        guard r.id == sessionId else { return }
        switch r.action {
        case "review":
            comments[r.id] = []
            status.stringValue = "Review sent to the agent."
            rebuild()
        case "revert": status.stringValue = "Reverted a hunk of \(r.file ?? "the file")."
        case "stage": status.stringValue = "Staged a hunk of \(r.file ?? "the file")."
        case "unstage": status.stringValue = "Unstaged a hunk of \(r.file ?? "the file")."
        default: break
        }
        if r.action != "review" { scheduleRefresh() }
    }

    /// The engine refused something we sent for `sessionId`.
    func failed(sessionId id: String, message: String) {
        guard id == sessionId else { return }
        if message.contains("stale") {
            status.stringValue = "That hunk changed under you — refreshed."
            scheduleRefresh()
        } else {
            status.stringValue = message
        }
    }

    /// `flow.diff.changed`: refetch if this is the diff on screen.
    func changed(sessionId id: String) {
        guard id == sessionId, window != nil, superview != nil else { return }
        scheduleRefresh()
    }

    private func scheduleRefresh() {
        guard !refreshScheduled else { return }
        refreshScheduled = true
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.3) { [weak self] in
            guard let self else { return }
            self.refreshScheduled = false
            self.request()
        }
    }

    private func updateSummary() {
        guard let d = diff else { summary.stringValue = ""; return }
        let n = d.files.count + d.filesOmitted
        var s = "\(n) file\(n == 1 ? "" : "s") · +\(d.added) −\(d.deleted)"
        if let t = d.turn { s += t.live ? " · turn in progress" : "" }
        s += " · \(d.from.label) → \(d.to.label)"
        summary.stringValue = s
    }

    private func syncBaseControl() {
        baseControl.selectedSegment = DiffBase.allCases.firstIndex(of: base) ?? 0
        modeControl.selectedSegment = split ? 1 : 0
        let n = comments[sessionId ?? ""]?.count ?? 0
        reviewButton.isHidden = n == 0
        reviewButton.title = "Send review to agent (\(n))"
        reviewButton.isEnabled = sessionKind != "shell"
        reviewButton.toolTip = sessionKind == "shell" ? "A review goes to an agent, not a shell" : nil
    }

    @objc private func baseChanged() {
        let b = DiffBase.allCases[max(0, min(baseControl.selectedSegment, DiffBase.allCases.count - 1))]
        setBase(b)
    }

    func setBase(_ b: DiffBase) {
        guard b != base, let id = sessionId else { syncBaseControl(); return }
        base = b
        baseChosen[id] = b
        diff = nil
        styled = [:]
        cursor = nil
        anchor = nil
        rebuild()
        syncBaseControl()
        request()
    }

    @objc private func modeChanged() { setSplit(modeControl.selectedSegment == 1) }
    @objc private func refreshClicked() { request() }
    @objc private func resized() { updateColumnWidth() }

    private func setSplit(_ on: Bool) {
        split = on
        UserDefaults.standard.set(on, forKey: "diffSplit")
        syncBaseControl()
        rebuild()
    }

    // MARK: rows

    private var sid: String { sessionId ?? "" }

    private func isViewed(_ f: DiffFile) -> Bool { viewed[sid]?.contains(DiffRules.viewedKey(f)) ?? false }

    private func isCollapsed(_ i: Int) -> Bool {
        guard let f = diff?.files[i] else { return false }
        if let t = toggles[sid]?[f.path] { return t }
        return DiffRules.display(f, viewed: isViewed(f)).collapsed
    }

    private func rebuild() {
        let keep = cursor.flatMap { rows.indices.contains($0) ? rows[$0] : nil }
        if let d = diff {
            let collapsed = d.files.indices.map(isCollapsed)
            let viewedFlags = d.files.map(isViewed)
            rows = DiffLayout.rows(d, split: split, collapsed: collapsed, viewed: viewedFlags, comments: comments[sid] ?? [])
            treeRows = DiffRules.tree(d.files.map(\.path))
            if rows.isEmpty || (d.files.isEmpty && d.note == nil) {
                rows.append(.banner("No changes."))
            }
        } else {
            rows = sessionId == nil ? [.banner("No session focused.")] : []
            treeRows = []
        }
        cursor = keep.flatMap { k in rows.firstIndex(of: k) } ?? cursor.map { min($0, max(rows.count - 1, 0)) }
        if rows.isEmpty { cursor = nil }
        anchor = nil
        updateColumnWidth()
        mainTable.reloadData()
        treeTable.reloadData()
        syncBaseControl()
    }

    private func updateColumnWidth() {
        guard let col = mainTable.tableColumns.first, let clip = mainTable.enclosingScrollView?.contentView else { return }
        let visible = clip.bounds.width
        var width = visible
        if !split, let d = diff {
            let longest = d.files.flatMap { $0.hunks.flatMap { $0.lines.map(\.text.count) } }.max() ?? 0
            width = max(visible, Self.gutter * 2 + 24 + CGFloat(longest) * charWidth + 24)
        }
        if abs(col.width - width) > 0.5 { col.width = width }
    }

    // MARK: table data

    func numberOfRows(in tableView: NSTableView) -> Int { tableView === treeTable ? treeRows.count : rows.count }

    func tableView(_ tableView: NSTableView, heightOfRow row: Int) -> CGFloat {
        if tableView === treeTable { return 22 }
        switch rows[row] {
        case .line, .pair: return 18
        case .hunkHeader: return 26
        case .fileHeader: return 32
        case .notice, .banner: return 26
        case let .comment(_, i):
            let text = comments[sid]?[safe: i]?.text ?? ""
            return CGFloat(max(1, text.components(separatedBy: "\n").count)) * 16 + 30
        }
    }

    func tableView(_ tableView: NSTableView, viewFor tableColumn: NSTableColumn?, row: Int) -> NSView? {
        tableView === treeTable ? treeCell(row) : mainCell(row)
    }

    static let gutter: CGFloat = 44

    private func mainCell(_ row: Int) -> NSView? {
        let d = diff ?? DiffPayload(base: base)
        guard rows.indices.contains(row) else { return nil }
        let r = rows[row]
        let highlight: DiffLineCell.Highlight = row == cursor ? .cursor : (inSelection(row) ? .selected : .none)
        switch r {
        case let .banner(text):
            let v = reuse("banner") { NoticeCell(frame: .zero) }
            v.configure(text: text, button: nil) {}
            return v
        case let .fileHeader(fi):
            let v = reuse("file") { FileHeaderCell(frame: .zero) }
            let f = d.files[fi]
            v.configure(f, collapsed: isCollapsed(fi), viewed: isViewed(f), cursor: row == cursor,
                        onToggle: { [weak self] in self?.toggleCollapse(fi) },
                        onViewed: { [weak self] in self?.toggleViewed(fi) })
            return v
        case let .notice(fi, text, _):
            let v = reuse("notice") { NoticeCell(frame: .zero) }
            let collapsed = isCollapsed(fi)
            v.configure(text: text, button: collapsed ? "Show" : nil) { [weak self] in self?.toggleCollapse(fi) }
            return v
        case let .hunkHeader(fi, hi):
            let v = reuse("hunk") { HunkHeaderCell(frame: .zero) }
            let h = d.files[fi].hunks[hi]
            v.configure(h, canStage: base == .uncommitted && !d.files[fi].binary, cursor: row == cursor,
                        onStage: { [weak self] in self?.stageHunk(fi, hi) },
                        onRevert: { [weak self] in self?.confirmRevert(fi, hi) })
            return v
        case let .line(fi, hi, li):
            let v = reuse("line") { DiffLineCell(frame: .zero) }
            let l = d.files[fi].hunks[hi].lines[li]
            v.configureUnified(l, text: text(fi, hi, li), highlight: highlight, charWidth: charWidth, font: font)
            v.onClick = { [weak self] x, shift in self?.clicked(row: row, gutter: x < DiffView.gutter * 2, shift: shift) }
            return v
        case let .pair(fi, hi, left, right):
            let v = reuse("pair") { DiffLineCell(frame: .zero) }
            let h = d.files[fi].hunks[hi]
            v.configurePair(left: left.map { (h.lines[$0], text(fi, hi, $0)) }, right: right.map { (h.lines[$0], text(fi, hi, $0)) },
                            highlight: highlight, charWidth: charWidth, font: font)
            v.onClick = { [weak self] x, shift in
                guard let self else { return }
                let half = (self.mainTable.tableColumns.first?.width ?? 600) / 2
                let inGutter = x < DiffView.gutter || (x >= half && x < half + DiffView.gutter)
                self.clicked(row: row, gutter: inGutter, shift: shift)
            }
            return v
        case let .comment(_, ci):
            let v = reuse("comment") { CommentCell(frame: .zero) }
            if let c = comments[sid]?[safe: ci] {
                v.configure(c) { [weak self] in self?.removeComment(c.id) }
            }
            return v
        }
    }

    private func reuse<T: NSView>(_ id: String, _ make: () -> T) -> T {
        if let v = mainTable.makeView(withIdentifier: .init(id), owner: self) as? T { return v }
        let v = make()
        v.identifier = .init(id)
        return v
    }

    private func text(_ f: Int, _ h: Int, _ l: Int) -> NSAttributedString {
        let key = "\(f)/\(h)/\(l)"
        if let s = styled[key] { return s }
        guard let d = diff else { return NSAttributedString() }
        let s = DiffStyler.line(d.files[f].hunks[h].lines[l], legend: d.legend, font: font)
        styled[key] = s
        return s
    }

    private func treeCell(_ row: Int) -> NSView? {
        guard treeRows.indices.contains(row), let d = diff else { return nil }
        let t = treeRows[row]
        let v = (treeTable.makeView(withIdentifier: .init("tree"), owner: self) as? TreeCell) ?? {
            let c = TreeCell(frame: .zero)
            c.identifier = .init("tree")
            return c
        }()
        if let fi = t.file {
            let f = d.files[fi]
            v.configure(name: t.name, depth: t.depth, file: f, viewed: isViewed(f), current: cursorFile == fi)
        } else {
            v.configure(name: t.name, depth: t.depth, file: nil, viewed: false, current: false)
        }
        v.setAccessibilityIdentifier("diff.tree.\(t.path)")
        return v
    }

    @objc private func treeClicked() {
        let row = treeTable.clickedRow
        guard treeRows.indices.contains(row), let fi = treeRows[row].file else { return }
        jump(toFile: fi)
    }

    // MARK: cursor + selection

    private var cursorFile: Int? { cursor.flatMap { rows.indices.contains($0) ? rows[$0].file : nil } }

    private func inSelection(_ row: Int) -> Bool {
        guard let a = anchor, let c = cursor else { return false }
        return row >= min(a, c) && row <= max(a, c)
    }

    private func moveCursor(to row: Int?, extend: Bool = false) {
        guard let row, rows.indices.contains(row) else { return }
        let old = cursor
        if extend { anchor = anchor ?? cursor } else { anchor = nil }
        cursor = row
        mainTable.scrollRowToVisible(row)
        let lo = min(old ?? row, row, anchor ?? row), hi = max(old ?? row, row, anchor ?? row)
        if extend || old.map({ abs($0 - row) > 400 }) == false {
            mainTable.reloadData(forRowIndexes: IndexSet(integersIn: max(0, lo - 1)...min(rows.count - 1, hi + 1)), columnIndexes: [0])
        } else {
            mainTable.reloadData()
        }
        if old.flatMap({ rows.indices.contains($0) ? rows[$0].file : nil }) != rows[row].file { treeTable.reloadData() }
    }

    private func clicked(row: Int, gutter: Bool, shift: Bool) {
        window?.makeFirstResponder(mainTable)
        moveCursor(to: row, extend: shift)
        if gutter { comment() }
    }

    private func jump(toFile fi: Int) {
        guard let row = rows.firstIndex(of: .fileHeader(file: fi)) else { return }
        moveCursor(to: row)
        // The file's header at the top, not just somewhere on screen.
        mainTable.scroll(NSPoint(x: 0, y: mainTable.rect(ofRow: row).minY))
        window?.makeFirstResponder(mainTable)
    }

    // MARK: keys

    /// A key press in the main table: the keymap's view-scoped diff actions.
    func handleKey(_ event: NSEvent) -> Bool {
        guard let chord = KeyChord.from(event: event) else { return false }
        if chord == KeyChord("down") { perform(.diffNextLine); return true }
        if chord == KeyChord("up") { perform(.diffPreviousLine); return true }
        if chord == KeyChord("down", shift: true) || chord == KeyChord("up", shift: true) {
            let next = chord.key == "down" ? nextLineRow(after: cursor) : previousLineRow(before: cursor)
            moveCursor(to: next, extend: true)
            return true
        }
        guard let action = app.keymap.map.diffAction(for: chord) else { return false }
        perform(action)
        return true
    }

    func perform(_ action: FlowAction) {
        switch action {
        case .diffNextLine: moveCursor(to: nextLineRow(after: cursor))
        case .diffPreviousLine: moveCursor(to: previousLineRow(before: cursor))
        case .diffNextHunk, .diffPreviousHunk: moveHunk(forward: action == .diffNextHunk)
        case .diffNextFile, .diffPreviousFile: moveFile(forward: action == .diffNextFile)
        case .diffToggleViewed: if let f = cursorFile { toggleViewed(f) }
        case .diffComment: comment()
        case .diffRevertHunk: if let ch = cursorHunk { confirmRevert(ch.0, ch.1) }
        case .diffStageHunk: if let ch = cursorHunk { stageHunk(ch.0, ch.1) }
        case .diffToggleSplit: setSplit(!split)
        default: break
        }
    }

    private func nextLineRow(after row: Int?) -> Int? {
        let start = (row ?? -1) + 1
        guard start < rows.count else { return nil }
        return rows[start...].firstIndex { $0.isLine } ?? rows[start...].firstIndex { _ in true }
    }

    private func previousLineRow(before row: Int?) -> Int? {
        let end = (row ?? rows.count) - 1
        guard end >= 0, end < rows.count else { return nil }
        return rows[...end].lastIndex { $0.isLine }
    }

    private var cursorHunk: (Int, Int)? {
        guard let c = cursor, rows.indices.contains(c), let f = rows[c].file, let h = rows[c].hunk else { return nil }
        return (f, h)
    }

    private func moveHunk(forward: Bool) {
        guard let d = diff else { return }
        let order = DiffRules.fileOrder(d.files.map(\.path))
        let collapsed = d.files.indices.map(isCollapsed)
        let at = cursorHunk.map { (file: $0.0, hunk: $0.1) }
        guard let next = DiffRules.nextHunk(hunkCounts: d.files.map(\.hunks.count), order: order, collapsed: collapsed, at: at, forward: forward),
              let row = rows.firstIndex(of: .hunkHeader(file: next.file, hunk: next.hunk)) else { return }
        moveCursor(to: row)
    }

    private func moveFile(forward: Bool) {
        guard let d = diff else { return }
        let order = DiffRules.fileOrder(d.files.map(\.path))
        guard let next = DiffRules.nextFile(order: order, at: cursorFile, forward: forward) else { return }
        jump(toFile: next)
    }

    // MARK: actions

    private func toggleCollapse(_ fi: Int) {
        guard let f = diff?.files[fi] else { return }
        let now = !isCollapsed(fi)
        toggles[sid, default: [:]][f.path] = now
        if !now, f.hunks.isEmpty, f.hunksOmitted != nil { request(path: f.path) }
        rebuild()
    }

    private func toggleViewed(_ fi: Int) {
        guard let f = diff?.files[fi] else { return }
        let key = DiffRules.viewedKey(f)
        if viewed[sid, default: []].remove(key) == nil {
            viewed[sid, default: []].insert(key)
            // Viewing folds the file (GitHub's rule): drop any manual toggle.
            toggles[sid]?[f.path] = nil
            rebuild()
            if let next = diff.flatMap({ DiffRules.nextFile(order: DiffRules.fileOrder($0.files.map(\.path)), at: fi, forward: true) }) {
                jump(toFile: next)
            }
        } else {
            toggles[sid]?[f.path] = nil
            rebuild()
        }
    }

    private func confirmRevert(_ fi: Int, _ hi: Int) {
        guard let d = diff, let id = sessionId, let window else { return }
        let f = d.files[fi], h = f.hunks[hi]
        let alert = NSAlert()
        alert.alertStyle = .warning
        alert.messageText = "Revert this hunk of \(f.path)?"
        alert.informativeText = "\(h.header)\n\nThe worktree loses these lines. Other hunks are untouched."
        alert.addButton(withTitle: "Revert")
        alert.addButton(withTitle: "Cancel")
        alert.buttons[0].hasDestructiveAction = true
        alert.buttons[0].keyEquivalent = ""
        alert.buttons[0].setAccessibilityIdentifier("diff.revert.confirm")
        // Cancel is the default: a stray Return never discards work.
        alert.buttons[1].keyEquivalent = "\r"
        let base = self.base
        alert.beginSheetModal(for: window) { [weak self] response in
            guard response == .alertFirstButtonReturn else { return }
            self?.app.sendDiff(.diffRevert(id: id, base: base, hunkId: h.id), sessionId: id)
        }
    }

    private func stageHunk(_ fi: Int, _ hi: Int) {
        guard let d = diff, let id = sessionId else { return }
        guard base == .uncommitted else {
            status.stringValue = "Stage works on the Uncommitted view (the index is relative to HEAD)."
            return
        }
        let h = d.files[fi].hunks[hi]
        app.sendDiff(h.staged ? .diffUnstage(id: id, hunkId: h.id) : .diffStage(id: id, hunkId: h.id), sessionId: id)
    }

    /// Comment on the selected line or range (one hunk), or the file.
    private func comment() {
        guard let d = diff, let c = cursor, rows.indices.contains(c), let fi = rows[c].file else { return }
        let f = d.files[fi]
        var range: ClosedRange<Int>?
        var side = "new"
        var hunkId: String?
        if let hi = rows[c].hunk {
            let h = f.hunks[hi]
            hunkId = h.id
            let lo = min(anchor ?? c, c), hi2 = max(anchor ?? c, c)
            var news: [Int] = [], olds: [Int] = []
            for r in lo...hi2 where rows[r].file == fi && rows[r].hunk == hi {
                switch rows[r] {
                case let .line(_, _, li):
                    if let n = h.lines[li].new { news.append(n) } else if let o = h.lines[li].old { olds.append(o) }
                case let .pair(_, _, left, right):
                    if let li = right, let n = h.lines[li].new { news.append(n) } else if let li = left, let o = h.lines[li].old { olds.append(o) }
                default: break
                }
            }
            if let a = news.min(), let b = news.max() {
                range = a...b
            } else if let a = olds.min(), let b = olds.max() {
                range = a...b
                side = "old"
            }
        }
        let place = range.map { $0.lowerBound == $0.upperBound ? "\(f.path):\($0.lowerBound)" : "\(f.path):\($0.lowerBound)-\($0.upperBound)" } ?? f.path
        promptComment(title: "Comment on \(place)\(side == "old" ? " (old side)" : "")") { [weak self] text in
            guard let self, let text, !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else { return }
            self.comments[self.sid, default: []].append(DiffComment(file: f.path, hunkId: hunkId, range: range, side: side, text: text))
            self.anchor = nil
            self.rebuild()
        }
    }

    private func promptComment(title: String, done: @escaping (String?) -> Void) {
        guard let window else { return done(nil) }
        let alert = NSAlert()
        alert.messageText = title
        alert.informativeText = "Comments are sent to the agent together, as one review."
        let scroll = NSScrollView(frame: NSRect(x: 0, y: 0, width: 360, height: 90))
        let tv = NSTextView(frame: scroll.bounds)
        tv.isRichText = false
        tv.font = .systemFont(ofSize: 13)
        tv.autoresizingMask = [.width]
        tv.setAccessibilityIdentifier("diff.comment.text")
        scroll.documentView = tv
        scroll.hasVerticalScroller = true
        scroll.borderType = .bezelBorder
        alert.accessoryView = scroll
        alert.addButton(withTitle: "Add Comment")
        alert.addButton(withTitle: "Cancel")
        alert.buttons[0].setAccessibilityIdentifier("diff.comment.add")
        alert.window.initialFirstResponder = tv
        alert.beginSheetModal(for: window) { response in
            done(response == .alertFirstButtonReturn ? tv.string : nil)
        }
    }

    private func removeComment(_ id: UUID) {
        comments[sid]?.removeAll { $0.id == id }
        rebuild()
    }

    @objc private func sendReview() {
        guard let id = sessionId, let list = comments[id], !list.isEmpty else { return }
        status.stringValue = "Sending review…"
        app.sendDiff(.diffReview(id: id, base: base, comments: list), sessionId: id)
    }
}

extension Array {
    subscript(safe i: Int) -> Element? { indices.contains(i) ? self[i] : nil }
}

/// The main table: hands its keys to the viewer first.
final class DiffTableView: NSTableView {
    weak var owner: DiffView?
    override var acceptsFirstResponder: Bool { true }
    override func keyDown(with event: NSEvent) {
        if owner?.handleKey(event) == true { return }
        super.keyDown(with: event)
    }
}

// MARK: - cells

/// One diff line (unified) or one side-by-side pair, drawn — no subviews.
final class DiffLineCell: NSView {
    enum Highlight { case none, cursor, selected }

    var onClick: ((CGFloat, Bool) -> Void)?
    private var halves: [(line: DiffLine?, text: NSAttributedString?)] = []
    private var unified = true
    private var highlight: Highlight = .none
    private var charWidth: CGFloat = 7
    private var font = NSFont.monospacedSystemFont(ofSize: 12, weight: .regular)

    override var isFlipped: Bool { true }

    func configureUnified(_ l: DiffLine, text: NSAttributedString, highlight: Highlight, charWidth: CGFloat, font: NSFont) {
        unified = true
        halves = [(l, text)]
        apply(highlight, charWidth, font)
    }

    func configurePair(left: (DiffLine, NSAttributedString)?, right: (DiffLine, NSAttributedString)?, highlight: Highlight, charWidth: CGFloat, font: NSFont) {
        unified = false
        halves = [(left?.0, left?.1), (right?.0, right?.1)]
        apply(highlight, charWidth, font)
    }

    private func apply(_ h: Highlight, _ cw: CGFloat, _ f: NSFont) {
        highlight = h
        charWidth = cw
        font = f
        setAccessibilityElement(true)
        setAccessibilityRole(.staticText)
        setAccessibilityValue(halves.compactMap { $0.line?.text }.joined(separator: " │ "))
        needsDisplay = true
    }

    override func mouseDown(with event: NSEvent) {
        let p = convert(event.locationInWindow, from: nil)
        onClick?(p.x, event.modifierFlags.contains(.shift))
    }

    override func draw(_ dirtyRect: NSRect) {
        let gutter = DiffView.gutter
        let numberAttrs: [NSAttributedString.Key: Any] = [.font: NSFont.monospacedDigitSystemFont(ofSize: 10, weight: .regular), .foregroundColor: DiffPalette.overlay0]
        func bg(_ kind: DiffLineKind?) -> NSColor {
            switch kind {
            case .add: DiffPalette.addLine
            case .del: DiffPalette.delLine
            case .ctx: .clear
            case nil: DiffPalette.mantle
            }
        }
        func number(_ n: Int?, in rect: NSRect) {
            guard let n else { return }
            let s = NSAttributedString(string: String(n), attributes: numberAttrs)
            s.draw(at: NSPoint(x: rect.maxX - s.size().width - 6, y: rect.minY + 3))
        }
        if unified, let first = halves.first, let l = first.line {
            bg(l.kind).setFill()
            bounds.fill()
            number(l.old, in: NSRect(x: 0, y: 0, width: gutter, height: bounds.height))
            number(l.new, in: NSRect(x: gutter, y: 0, width: gutter, height: bounds.height))
            let marker = l.kind == .add ? "+" : (l.kind == .del ? "−" : " ")
            NSAttributedString(string: marker, attributes: [.font: font, .foregroundColor: l.kind == .add ? DiffPalette.green : DiffPalette.red])
                .draw(at: NSPoint(x: gutter * 2 + 4, y: 1))
            first.text?.draw(at: NSPoint(x: gutter * 2 + 18, y: 1))
        } else {
            let half = bounds.width / 2
            for (i, h) in halves.enumerated() {
                let rect = NSRect(x: CGFloat(i) * half, y: 0, width: half, height: bounds.height)
                bg(h.line?.kind).setFill()
                rect.fill()
                guard let l = h.line else { continue }
                number(i == 0 ? l.old : l.new, in: NSRect(x: rect.minX, y: 0, width: gutter, height: bounds.height))
                NSGraphicsContext.saveGraphicsState()
                NSRect(x: rect.minX + gutter + 6, y: 0, width: half - gutter - 8, height: bounds.height).clip()
                h.text?.draw(at: NSPoint(x: rect.minX + gutter + 6, y: 1))
                NSGraphicsContext.restoreGraphicsState()
            }
            DiffPalette.surface0.setFill()
            NSRect(x: half - 0.5, y: 0, width: 1, height: bounds.height).fill()
        }
        switch highlight {
        case .cursor:
            DiffPalette.cursor.setFill()
            bounds.fill(using: .sourceOver)
            DiffPalette.blue.setFill()
            NSRect(x: 0, y: 0, width: 2, height: bounds.height).fill()
        case .selected:
            DiffPalette.selected.setFill()
            bounds.fill(using: .sourceOver)
        case .none: break
        }
    }
}

/// File header: chevron, status badge, path, counts, Viewed.
final class FileHeaderCell: NSView {
    private let chevron = NSButton(title: "", target: nil, action: nil)
    private let badge = NSTextField(labelWithString: "")
    private let path = NSTextField(labelWithString: "")
    private let counts = NSTextField(labelWithString: "")
    private let viewedBox = NSButton(checkboxWithTitle: "Viewed", target: nil, action: nil)
    private var onToggle: (() -> Void)?
    private var onViewed: (() -> Void)?

    override init(frame: NSRect) {
        super.init(frame: frame)
        wantsLayer = true
        chevron.isBordered = false
        chevron.target = self
        chevron.action = #selector(toggle)
        viewedBox.target = self
        viewedBox.action = #selector(viewedClicked)
        viewedBox.controlSize = .small
        badge.font = Theme.monoNSFont(size: 11)
        path.font = Theme.monoNSFont(size: 12)
        path.textColor = DiffPalette.text
        path.lineBreakMode = .byTruncatingMiddle
        counts.font = Theme.monoNSFont(size: 11)
        path.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        let row = NSStackView(views: [chevron, badge, path, counts, NSView(), viewedBox])
        row.orientation = .horizontal
        row.spacing = 8
        row.edgeInsets = NSEdgeInsets(top: 0, left: 8, bottom: 0, right: 12)
        row.translatesAutoresizingMaskIntoConstraints = false
        addSubview(row)
        NSLayoutConstraint.activate([
            row.leadingAnchor.constraint(equalTo: leadingAnchor), row.trailingAnchor.constraint(equalTo: trailingAnchor),
            row.topAnchor.constraint(equalTo: topAnchor), row.bottomAnchor.constraint(equalTo: bottomAnchor),
        ])
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { nil }

    func configure(_ f: DiffFile, collapsed: Bool, viewed: Bool, cursor: Bool, onToggle: @escaping () -> Void, onViewed: @escaping () -> Void) {
        self.onToggle = onToggle
        self.onViewed = onViewed
        layer?.backgroundColor = (cursor ? DiffPalette.surface1 : DiffPalette.surface0).cgColor
        chevron.title = collapsed ? "▸" : "▾"
        badge.stringValue = f.badge
        badge.textColor = DiffPalette.badgeColor(f.status)
        path.stringValue = f.oldPath.map { "\($0) → \(f.path)" } ?? f.path
        let c = NSMutableAttributedString(string: "+\(f.added)", attributes: [.foregroundColor: DiffPalette.green, .font: Theme.monoNSFont(size: 11)])
        c.append(NSAttributedString(string: " −\(f.deleted)", attributes: [.foregroundColor: DiffPalette.red, .font: Theme.monoNSFont(size: 11)]))
        if let noise = f.noise { c.append(NSAttributedString(string: "  \(noise)", attributes: [.foregroundColor: DiffPalette.overlay0, .font: Theme.monoNSFont(size: 11)])) }
        counts.attributedStringValue = c
        viewedBox.state = viewed ? .on : .off
        setAccessibilityElement(true)
        setAccessibilityRole(.group)
        setAccessibilityIdentifier("diff.file.\(f.path)")
        setAccessibilityLabel(f.path)
    }

    @objc private func toggle() { onToggle?() }
    @objc private func viewedClicked() { onViewed?() }
}

/// Hunk header: the `@@` line, staged tag, Stage/Unstage and Revert.
final class HunkHeaderCell: NSView {
    private let header = NSTextField(labelWithString: "")
    private let staged = NSTextField(labelWithString: "staged")
    private let stage = NSButton(title: "Stage", target: nil, action: nil)
    private let revert = NSButton(title: "Revert", target: nil, action: nil)
    private var onStage: (() -> Void)?
    private var onRevert: (() -> Void)?

    override init(frame: NSRect) {
        super.init(frame: frame)
        wantsLayer = true
        header.font = Theme.monoNSFont(size: 11)
        header.textColor = DiffPalette.sky
        header.lineBreakMode = .byTruncatingTail
        header.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        staged.font = .systemFont(ofSize: 10, weight: .semibold)
        staged.textColor = DiffPalette.green
        for b in [stage, revert] {
            b.bezelStyle = .rounded
            b.controlSize = .mini
            b.target = self
        }
        stage.action = #selector(stageClicked)
        revert.action = #selector(revertClicked)
        let row = NSStackView(views: [header, NSView(), staged, stage, revert])
        row.orientation = .horizontal
        row.spacing = 8
        row.edgeInsets = NSEdgeInsets(top: 0, left: 12, bottom: 0, right: 12)
        row.translatesAutoresizingMaskIntoConstraints = false
        addSubview(row)
        NSLayoutConstraint.activate([
            row.leadingAnchor.constraint(equalTo: leadingAnchor), row.trailingAnchor.constraint(equalTo: trailingAnchor),
            row.topAnchor.constraint(equalTo: topAnchor), row.bottomAnchor.constraint(equalTo: bottomAnchor),
        ])
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { nil }

    func configure(_ h: DiffHunk, canStage: Bool, cursor: Bool, onStage: @escaping () -> Void, onRevert: @escaping () -> Void) {
        self.onStage = onStage
        self.onRevert = onRevert
        layer?.backgroundColor = (cursor ? DiffPalette.surface0 : DiffPalette.mantle).cgColor
        header.stringValue = h.header
        staged.isHidden = !h.staged
        stage.isHidden = !canStage
        stage.title = h.staged ? "Unstage" : "Stage"
        stage.setAccessibilityIdentifier("diff.hunk.stage.\(h.id)")
        revert.setAccessibilityIdentifier("diff.hunk.revert.\(h.id)")
        revert.isEnabled = !h.truncated
    }

    @objc private func stageClicked() { onStage?() }
    @objc private func revertClicked() { onRevert?() }
}

/// A line of text with an optional button (collapsed files, notes, banners).
final class NoticeCell: NSView {
    private let label = NSTextField(labelWithString: "")
    private let button = NSButton(title: "", target: nil, action: nil)
    private var onButton: (() -> Void)?

    override init(frame: NSRect) {
        super.init(frame: frame)
        label.font = .systemFont(ofSize: 11)
        label.textColor = DiffPalette.overlay2
        label.lineBreakMode = .byTruncatingTail
        button.bezelStyle = .rounded
        button.controlSize = .mini
        button.target = self
        button.action = #selector(clicked)
        let row = NSStackView(views: [label, button, NSView()])
        row.orientation = .horizontal
        row.spacing = 8
        row.edgeInsets = NSEdgeInsets(top: 0, left: 32, bottom: 0, right: 12)
        row.translatesAutoresizingMaskIntoConstraints = false
        addSubview(row)
        NSLayoutConstraint.activate([
            row.leadingAnchor.constraint(equalTo: leadingAnchor), row.trailingAnchor.constraint(equalTo: trailingAnchor),
            row.topAnchor.constraint(equalTo: topAnchor), row.bottomAnchor.constraint(equalTo: bottomAnchor),
        ])
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { nil }

    func configure(text: String, button title: String?, action: @escaping () -> Void) {
        label.stringValue = text
        button.isHidden = title == nil
        button.title = title ?? ""
        onButton = action
    }

    @objc private func clicked() { onButton?() }
}

/// A pending review comment.
final class CommentCell: NSView {
    private let label = NSTextField(wrappingLabelWithString: "")
    private let remove = NSButton(title: "Remove", target: nil, action: nil)
    private var onRemove: (() -> Void)?

    override init(frame: NSRect) {
        super.init(frame: frame)
        wantsLayer = true
        layer?.backgroundColor = DiffPalette.surface0.cgColor
        label.font = .systemFont(ofSize: 12)
        label.textColor = DiffPalette.text
        remove.bezelStyle = .rounded
        remove.controlSize = .mini
        remove.target = self
        remove.action = #selector(removeClicked)
        let row = NSStackView(views: [label, NSView(), remove])
        row.orientation = .horizontal
        row.alignment = .top
        row.edgeInsets = NSEdgeInsets(top: 6, left: 96, bottom: 6, right: 12)
        row.translatesAutoresizingMaskIntoConstraints = false
        addSubview(row)
        NSLayoutConstraint.activate([
            row.leadingAnchor.constraint(equalTo: leadingAnchor), row.trailingAnchor.constraint(equalTo: trailingAnchor),
            row.topAnchor.constraint(equalTo: topAnchor), row.bottomAnchor.constraint(equalTo: bottomAnchor),
        ])
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { nil }

    func configure(_ c: DiffComment, onRemove: @escaping () -> Void) {
        self.onRemove = onRemove
        label.stringValue = c.text
        setAccessibilityIdentifier("diff.comment")
    }

    @objc private func removeClicked() { onRemove?() }
}

/// A file-tree row: indent, badge, name, counts, viewed check.
final class TreeCell: NSView {
    private let label = NSTextField(labelWithString: "")
    private var leading: NSLayoutConstraint!

    override init(frame: NSRect) {
        super.init(frame: frame)
        wantsLayer = true
        label.lineBreakMode = .byTruncatingMiddle
        label.translatesAutoresizingMaskIntoConstraints = false
        addSubview(label)
        leading = label.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 8)
        NSLayoutConstraint.activate([
            leading, label.trailingAnchor.constraint(lessThanOrEqualTo: trailingAnchor, constant: -6), label.centerYAnchor.constraint(equalTo: centerYAnchor),
        ])
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { nil }

    func configure(name: String, depth: Int, file: DiffFile?, viewed: Bool, current: Bool) {
        leading.constant = 8 + CGFloat(depth) * 12
        layer?.backgroundColor = (current ? DiffPalette.surface0 : .clear).cgColor
        let s = NSMutableAttributedString()
        let small = Theme.monoNSFont(size: 11)
        if let f = file {
            s.append(NSAttributedString(string: f.badge + " ", attributes: [.font: small, .foregroundColor: DiffPalette.badgeColor(f.status)]))
            s.append(NSAttributedString(string: name, attributes: [.font: NSFont.systemFont(ofSize: 12), .foregroundColor: viewed ? DiffPalette.overlay0 : DiffPalette.text]))
            s.append(NSAttributedString(string: "  +\(f.added)", attributes: [.font: small, .foregroundColor: DiffPalette.green]))
            s.append(NSAttributedString(string: " −\(f.deleted)", attributes: [.font: small, .foregroundColor: DiffPalette.red]))
            if viewed { s.append(NSAttributedString(string: "  ✓", attributes: [.font: small, .foregroundColor: DiffPalette.teal])) }
        } else {
            s.append(NSAttributedString(string: name + "/", attributes: [.font: NSFont.systemFont(ofSize: 12), .foregroundColor: DiffPalette.subtext]))
        }
        label.attributedStringValue = s
    }
}

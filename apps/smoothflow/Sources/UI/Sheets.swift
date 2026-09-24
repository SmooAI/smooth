import AppKit
import SwiftUI

/// The kind picker every sheet shares (th-0f6126): the engine's harness list
/// (`flow.hello` / `flow.harnesses`) in the user's order, hidden ones already
/// gone, a missing binary shown disabled with the engine's reason, and Shell
/// last. Never a hard-coded kind.
struct HarnessPicker: View {
    @ObservedObject var store: FlowStore
    @Binding var kind: String
    var includeShell = true

    var body: some View {
        Picker("Kind", selection: $kind) {
            ForEach(store.harnesses) { h in
                Text(h.pickerLabel).tag(h.name).selectionDisabled(!h.installed)
            }
            if includeShell { Text("Shell").tag("shell") }
        }
        .pickerStyle(.menu)
    }

    /// The first launchable harness (or shell) — what a fresh sheet selects.
    static func defaultKind(_ store: FlowStore, includeShell: Bool = true) -> String {
        store.harnesses.first { $0.installed }?.name ?? (includeShell ? "shell" : store.harnesses.first?.name ?? "claude")
    }
}

/// The doctor's one fix command under a harness picker, copyable (th-51bf88).
/// SmoothFlow never runs it: installing, signing in or trusting hooks is the
/// user's call.
struct HarnessFixLine: View {
    let fix: String
    @State private var copied = false

    var body: some View {
        HStack(spacing: 6) {
            Text(fix).font(Theme.mono(.caption)).textSelection(.enabled).lineLimit(2).truncationMode(.middle)
            Button(copied ? "Copied" : "Copy") {
                NSPasteboard.general.clearContents()
                NSPasteboard.general.setString(fix, forType: .string)
                copied = true
            }
            .buttonStyle(.link).font(.caption)
            .accessibilityIdentifier("harness-fix-copy")
        }
        .accessibilityIdentifier("harness-fix")
        .onChange(of: fix) { _, _ in copied = false }
    }
}

/// Where the session runs (th-145e6b): type to search every git checkout
/// under `~` (the daemon's index, `GET /api/flow/repos`), type or paste a
/// path, or Browse… for any folder. ↑/↓ move through the matches, Return
/// picks. Picking re-infers the pearl, branch and title for that directory.
struct DirectoryField: View {
    @ObservedObject var app: AppController
    @Binding var chosen: String
    /// The directory in effect before anything is picked (the inferred one).
    var current: String
    var onPick: (String) -> Void

    @State private var query = ""
    @State private var results = RepoList()
    @State private var highlighted = 0
    @FocusState private var focused: Bool

    private var effective: String { chosen.isEmpty ? current : chosen }

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack(spacing: 6) {
                Image(systemName: "folder").foregroundStyle(Color(Theme.muted))
                TextField(effective.isEmpty ? "Directory — type to search your repos" : DirectoryPicking.abbreviate(effective), text: $query)
                    .textFieldStyle(.roundedBorder)
                    .font(Theme.mono(.body))
                    .focused($focused)
                    .onSubmit(pickHighlighted)
                    .onKeyPress(.downArrow) { highlighted = DirectoryPicking.moved(highlighted, by: 1, count: results.repos.count); return .handled }
                    .onKeyPress(.upArrow) { highlighted = DirectoryPicking.moved(highlighted, by: -1, count: results.repos.count); return .handled }
                    .onKeyPress(.escape) {
                        guard !query.isEmpty else { return .ignored }
                        query = ""
                        return .handled
                    }
                    .accessibilityIdentifier("newsession.directory")
                Button("Browse…", action: browse).accessibilityIdentifier("newsession.directory.browse")
            }
            if focused || !query.isEmpty { matches }
        }
        .task(id: query) {
            // A short debounce: the index answers in milliseconds, typing is faster.
            try? await Task.sleep(for: .milliseconds(90))
            guard !Task.isCancelled else { return }
            results = await app.searchRepos(query)
            highlighted = 0
        }
    }

    @ViewBuilder private var matches: some View {
        VStack(alignment: .leading, spacing: 0) {
            ForEach(Array(results.repos.prefix(8).enumerated()), id: \.element.id) { i, repo in
                Button { pick(repo.path) } label: {
                    HStack(spacing: 8) {
                        Text(repo.name).font(.body.weight(.medium))
                        if let b = repo.branch { Text(b).font(Theme.mono(.caption)).foregroundStyle(Color(Theme.muted)) }
                        Spacer()
                        Text(repo.shortPath).font(Theme.mono(.caption)).foregroundStyle(Color(Theme.faint)).lineLimit(1).truncationMode(.head)
                    }
                    .padding(.horizontal, 8).padding(.vertical, 4)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .background(RoundedRectangle(cornerRadius: 4).fill(i == highlighted ? Color.accentColor.opacity(0.18) : .clear))
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .accessibilityIdentifier("newsession.directory.match.\(repo.name)")
            }
            if results.repos.isEmpty {
                Text(emptyNote).font(.caption).foregroundStyle(Color(Theme.faint)).padding(.horizontal, 8).padding(.vertical, 4)
            } else if results.scanning {
                Text("indexing ~ — more may appear").font(.caption2).foregroundStyle(Color(Theme.faint)).padding(.horizontal, 8)
            }
        }
        .padding(4)
        .background(RoundedRectangle(cornerRadius: 6).fill(Color(NSColor.controlBackgroundColor)))
    }

    private var emptyNote: String {
        if DirectoryPicking.expandedPath(query) != nil { return "Return to use this path" }
        if results.scanning || !results.indexed { return "indexing your repos under ~…" }
        return "no repo matches — Browse… for any folder"
    }

    private func pickHighlighted() {
        if let path = DirectoryPicking.expandedPath(query) {
            pick(path)
        } else if results.repos.indices.contains(highlighted) {
            pick(results.repos[highlighted].path)
        }
    }

    private func pick(_ path: String) {
        chosen = path
        query = ""
        focused = false
        onPick(path)
    }

    private func browse() {
        let panel = NSOpenPanel()
        panel.canChooseDirectories = true
        panel.canChooseFiles = false
        panel.allowsMultipleSelection = false
        panel.canCreateDirectories = true
        panel.prompt = "Use Folder"
        if !effective.isEmpty { panel.directoryURL = URL(fileURLWithPath: effective) }
        if panel.runModal() == .OK, let url = panel.url { pick(url.path) }
    }
}

/// ⌘N — `flow.new`. Zero friction (th-c103c1): pick a kind, hit Start.
/// The pearl, Jira key, worktree and title are INFERRED from where the work
/// already is — shown, not demanded — and every one of them is overridable
/// behind the disclosure. Start is never blocked on a missing pearl.
struct NewSessionSheet: View {
    @ObservedObject var app: AppController
    @State private var kind = ""
    @State private var prompt = ""
    @State private var showOverrides = false
    /// The directory the user picked (th-145e6b); empty means "the inferred one".
    @State private var directory = ""
    @State private var pearlId = ""
    @State private var title = ""
    var dismiss: () -> Void = {}

    private var selected: HarnessInfo? { app.store.harnesses.first { $0.name == kind } }
    private var launchable: Bool { kind == "shell" || selected?.installed == true }
    private var context: InferredContext? { app.inferred }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("New session").font(.title3.bold())
            HarnessPicker(store: app.store, kind: $kind)
            if let h = selected, !h.installed {
                Text(h.reason ?? "not installed").font(.caption).foregroundStyle(Color(Theme.muted))
                if let fix = h.health?.fix { HarnessFixLine(fix: fix) }
            } else if let h = selected, h.isDegraded, let health = h.health {
                // th-51bf88: still startable, but say what will be missing and how to fix it.
                Text(health.reason ?? "needs setup").font(.caption).foregroundStyle(Color(Theme.amber))
                if let fix = health.fix { HarnessFixLine(fix: fix) }
            } else if let h = selected, h.stateSource == "native" {
                Text("native state — \(h.displayName) reports its own turns to the engine").font(.caption2).foregroundStyle(Color(Theme.faint))
            }
            DirectoryField(app: app, chosen: $directory, current: context?.worktree ?? "") { path in
                Task { await app.loadInference(cwd: path) }
            }
            inferredContext
            TextField("Prompt (optional)", text: $prompt, axis: .vertical).lineLimit(3...6)
            DisclosureGroup("Override context", isExpanded: $showOverrides) {
                VStack(alignment: .leading, spacing: 8) {
                    TextField("Pearl id (th-xxxxxx) — with no worktree, the engine creates one", text: $pearlId).font(Theme.mono(.body))
                    TextField("Title", text: $title)
                }.padding(.top, 6)
            }
            .font(.caption)
            HStack {
                Spacer()
                Button("Cancel") { dismiss() }.keyboardShortcut(.cancelAction)
                Button("Start") { start() }.keyboardShortcut(.defaultAction).disabled(!launchable)
            }
        }
        .padding(20)
        .frame(width: 520)
        .onAppear { if kind.isEmpty { kind = HarnessPicker.defaultKind(app.store) } }
        .task { await app.loadInference(cwd: app.inferSeedCwd) }
    }

    /// What the session will inherit if you just press Start.
    @ViewBuilder private var inferredContext: some View {
        if let c = context {
            VStack(alignment: .leading, spacing: 2) {
                Text(c.title).font(.body.weight(.medium)).lineLimit(1)
                Text(c.summary).font(Theme.mono(.caption)).foregroundStyle(Color(Theme.muted)).lineLimit(2)
                if !c.isGit {
                    Text("not a git worktree — no pearl, no branch").font(.caption2).foregroundStyle(Color(Theme.faint))
                } else if c.pearlId == nil {
                    Text("no pearl here — starting anyway is fine").font(.caption2).foregroundStyle(Color(Theme.faint))
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(8)
            .background(RoundedRectangle(cornerRadius: 6).fill(Color(NSColor.controlBackgroundColor)))
        } else {
            Text("reading the context…").font(.caption).foregroundStyle(Color(Theme.faint))
        }
    }

    /// An override wins; otherwise the inferred value goes on the wire, so the
    /// session records exactly the context the dialog showed.
    private func start() {
        let c = context
        app.newSession(NewSession(
            kind: kind,
            worktree: directory.nilIfEmpty ?? (pearlId.nilIfEmpty == nil ? c?.worktree.nilIfEmpty : nil),
            project: c?.project.nilIfEmpty,
            pearlId: pearlId.nilIfEmpty ?? c?.pearlId,
            prompt: prompt.nilIfEmpty,
            argv: nil,
            title: title.nilIfEmpty
        ))
        dismiss()
    }
}

/// Fan out: one prompt, N candidates → `flow.fanout.new`; compare → `flow.fanout.pick`.
struct FanOutSheet: View {
    @ObservedObject var store: FlowStore
    @ObservedObject var app: AppController
    var existingId: String?
    @State private var prompt = ""
    @State private var pearlId = ""
    @State private var candidates: [FanOutCandidate] = []
    var dismiss: () -> Void = {}

    /// One candidate per visible, installed harness — the engine's list, in
    /// the user's order (th-0f6126); the Claude Code row gets the two flagship
    /// models when it is there.
    static func defaultCandidates(_ harnesses: [HarnessInfo]) -> [FanOutCandidate] {
        var out: [FanOutCandidate] = []
        for h in harnesses where h.installed {
            if h.name == "claude" {
                out.append(FanOutCandidate(kind: h.name, model: "opus-5", label: "\(h.name) · opus 5"))
                out.append(FanOutCandidate(kind: h.name, model: "fable-5.1", label: "\(h.name) · fable 5.1"))
            } else {
                out.append(FanOutCandidate(kind: h.name, model: nil, label: h.name))
            }
        }
        return out
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Fan out · one prompt, N worktrees, pick the winner").font(.title3.bold())
            if let f = existingId.flatMap({ store.fanOuts[$0] }) {
                compare(f)
            } else {
                compose
            }
        }
        .padding(20)
        .frame(width: 720)
        .onAppear { if candidates.isEmpty { candidates = Self.defaultCandidates(store.harnesses) } }
    }

    private var compose: some View {
        VStack(alignment: .leading, spacing: 10) {
            TextField("Pearl id", text: $pearlId).font(Theme.mono(.body))
            TextField("Prompt", text: $prompt, axis: .vertical).lineLimit(3...8)
            Text("CANDIDATES").font(.caption).foregroundStyle(Color(Theme.muted))
            ForEach(candidates.indices, id: \.self) { i in
                HStack {
                    HarnessPicker(store: store, kind: $candidates[i].kind, includeShell: false).labelsHidden().frame(width: 150)
                    TextField("model", text: Binding(get: { candidates[i].model ?? "" }, set: { candidates[i].model = $0.nilIfEmpty })).frame(width: 140)
                    TextField("label", text: $candidates[i].label)
                    Button("−") { candidates.remove(at: i) }
                }.font(Theme.mono(.body))
            }
            Button("+ add") {
                let k = HarnessPicker.defaultKind(store, includeShell: false)
                candidates.append(FanOutCandidate(kind: k, model: nil, label: k))
            }
            Text("one child pearl each · stale-base guard is the engine's").font(.caption2).foregroundStyle(Color(Theme.faint))
            HStack {
                Spacer()
                Button("Cancel") { dismiss() }.keyboardShortcut(.cancelAction)
                Button("Fan out") { app.fanoutNew(prompt: prompt, pearlId: pearlId.nilIfEmpty, candidates: candidates); dismiss() }
                    .keyboardShortcut(.defaultAction)
                    .disabled(prompt.isEmpty || candidates.isEmpty || candidates.contains { c in store.harnesses.first { $0.name == c.kind }?.installed != true })
            }
        }
    }

    private func compare(_ f: FanOut) -> some View {
        let cands = (store.fanOutCandidates[f.id] ?? []).compactMap { store.sessions[$0] }
        return VStack(alignment: .leading, spacing: 10) {
            Text(f.prompt).font(.body)
            if let b = f.baseCommit { Text("base: \(b)").font(Theme.mono(.caption)).foregroundStyle(Color(Theme.muted)) }
            HStack(alignment: .top, spacing: 12) {
                ForEach(Array(cands.enumerated()), id: \.element.id) { i, s in
                    VStack(alignment: .leading, spacing: 6) {
                        HStack { StateDot(session: s); Text("\(Character(UnicodeScalar(65 + i)!)) · \(s.title)").font(.headline) }
                        Text(Theme.stateLabel(s) + " · " + Theme.relative(s.updatedAt)).font(.caption)
                        Text(s.worktree).font(Theme.mono(.caption)).foregroundStyle(Color(Theme.muted)).lineLimit(1)
                        if let d = app.handoffs[s.id]?.handoff?.dirty { Text("\(d.count) files").font(.caption) }
                        HStack {
                            Button("Pick winner") { app.fanoutPick(fanOutId: f.id, winner: s.id); dismiss() }
                                .disabled(s.state != .done || f.winnerSessionId != nil)
                            Button("Open") { app.focus(s.id); dismiss() }
                        }.controlSize(.small)
                    }
                    .padding(10)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .background(RoundedRectangle(cornerRadius: 8).fill(Color(NSColor.controlBackgroundColor)))
                    .task { await app.loadHandoff(for: s.id) }
                }
            }
            Text("Pick winner → th worktree merge · the losers' worktrees and child pearls are GC'd, transcripts kept.")
                .font(.caption2).foregroundStyle(Color(Theme.faint))
            HStack { Spacer(); Button("Close") { dismiss() }.keyboardShortcut(.cancelAction) }
        }
    }
}

extension String {
    var nilIfEmpty: String? { isEmpty ? nil : self }
}

extension NSViewController {
    /// Present a SwiftUI sheet from an AppKit controller; the view gets a
    /// `dismiss` closure (SwiftUI's Environment dismiss does not end
    /// NSViewController sheets reliably).
    @MainActor
    func presentSheet<V: View>(_ make: (@escaping () -> Void) -> V) {
        var host: NSHostingController<V>?
        let view = make { [weak self] in if let host { self?.dismiss(host) } }
        host = NSHostingController(rootView: view)
        presentAsSheet(host!)
    }
}

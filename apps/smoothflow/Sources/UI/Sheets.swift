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

/// ⌘N — `flow.new`.
struct NewSessionSheet: View {
    @ObservedObject var app: AppController
    @State private var kind = ""
    @State private var worktree = ""
    @State private var pearlId = ""
    @State private var prompt = ""
    @State private var title = ""
    var dismiss: () -> Void = {}

    private var selected: HarnessInfo? { app.store.harnesses.first { $0.name == kind } }
    private var launchable: Bool { kind == "shell" || selected?.installed == true }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("New session").font(.title3.bold())
            HarnessPicker(store: app.store, kind: $kind)
            if let h = selected, !h.installed {
                Text(h.reason ?? "not installed").font(.caption).foregroundStyle(Color(Theme.muted))
            } else if let h = selected, h.stateSource == "native" {
                Text("native state — \(h.displayName) reports its own turns to the engine").font(.caption2).foregroundStyle(Color(Theme.faint))
            }
            TextField("Pearl id (th-xxxxxx) — the engine creates the worktree", text: $pearlId).font(.body.monospaced())
            TextField("Worktree path (blank = derive from pearl / cwd)", text: $worktree).font(.body.monospaced())
            TextField("Title (optional)", text: $title)
            TextField("Prompt (optional)", text: $prompt, axis: .vertical).lineLimit(3...6)
            HStack {
                Spacer()
                Button("Cancel") { dismiss() }.keyboardShortcut(.cancelAction)
                Button("Start") {
                    app.newSession(NewSession(kind: kind, worktree: worktree.nilIfEmpty, project: nil, pearlId: pearlId.nilIfEmpty,
                                              prompt: prompt.nilIfEmpty, argv: nil, title: title.nilIfEmpty))
                    dismiss()
                }.keyboardShortcut(.defaultAction).disabled(!launchable)
            }
        }
        .padding(20)
        .frame(width: 520)
        .onAppear { if kind.isEmpty { kind = HarnessPicker.defaultKind(app.store) } }
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
            TextField("Pearl id", text: $pearlId).font(.body.monospaced())
            TextField("Prompt", text: $prompt, axis: .vertical).lineLimit(3...8)
            Text("CANDIDATES").font(.caption).foregroundStyle(Color(Theme.muted))
            ForEach(candidates.indices, id: \.self) { i in
                HStack {
                    HarnessPicker(store: store, kind: $candidates[i].kind, includeShell: false).labelsHidden().frame(width: 150)
                    TextField("model", text: Binding(get: { candidates[i].model ?? "" }, set: { candidates[i].model = $0.nilIfEmpty })).frame(width: 140)
                    TextField("label", text: $candidates[i].label)
                    Button("−") { candidates.remove(at: i) }
                }.font(.body.monospaced())
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
            if let b = f.baseCommit { Text("base: \(b)").font(.caption.monospaced()).foregroundStyle(Color(Theme.muted)) }
            HStack(alignment: .top, spacing: 12) {
                ForEach(Array(cands.enumerated()), id: \.element.id) { i, s in
                    VStack(alignment: .leading, spacing: 6) {
                        HStack { StateDot(session: s); Text("\(Character(UnicodeScalar(65 + i)!)) · \(s.title)").font(.headline) }
                        Text(Theme.stateLabel(s) + " · " + Theme.relative(s.updatedAt)).font(.caption)
                        Text(s.worktree).font(.caption.monospaced()).foregroundStyle(Color(Theme.muted)).lineLimit(1)
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

import AppKit
import SwiftUI

/// ⌘N — `flow.new`.
struct NewSessionSheet: View {
    @ObservedObject var app: AppController
    @State private var kind = "claude"
    @State private var worktree = ""
    @State private var pearlId = ""
    @State private var prompt = ""
    @State private var title = ""
    var dismiss: () -> Void = {}

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("New session").font(.title3.bold())
            Picker("Kind", selection: $kind) {
                Text("Claude Code").tag("claude")
                Text("Codex").tag("codex")
                Text("OpenCode").tag("opencode")
                Text("Shell").tag("shell")
            }.pickerStyle(.segmented)
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
                }.keyboardShortcut(.defaultAction)
            }
        }
        .padding(20)
        .frame(width: 520)
    }
}

/// Fan out: one prompt, N candidates → `flow.fanout.new`; compare → `flow.fanout.pick`.
struct FanOutSheet: View {
    @ObservedObject var store: FlowStore
    @ObservedObject var app: AppController
    var existingId: String?
    @State private var prompt = ""
    @State private var pearlId = ""
    @State private var candidates: [FanOutCandidate] = [
        FanOutCandidate(kind: "claude", model: "opus-5", label: "claude · opus 5"),
        FanOutCandidate(kind: "claude", model: "fable-5.1", label: "claude · fable 5.1"),
        FanOutCandidate(kind: "codex", model: nil, label: "codex"),
    ]
    var dismiss: () -> Void = {}

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
    }

    private var compose: some View {
        VStack(alignment: .leading, spacing: 10) {
            TextField("Pearl id", text: $pearlId).font(.body.monospaced())
            TextField("Prompt", text: $prompt, axis: .vertical).lineLimit(3...8)
            Text("CANDIDATES").font(.caption).foregroundStyle(Color(Theme.muted))
            ForEach(candidates.indices, id: \.self) { i in
                HStack {
                    TextField("kind", text: $candidates[i].kind).frame(width: 90)
                    TextField("model", text: Binding(get: { candidates[i].model ?? "" }, set: { candidates[i].model = $0.nilIfEmpty })).frame(width: 140)
                    TextField("label", text: $candidates[i].label)
                    Button("−") { candidates.remove(at: i) }
                }.font(.body.monospaced())
            }
            Button("+ add") { candidates.append(FanOutCandidate(kind: "claude", model: nil, label: "claude")) }
            Text("one child pearl each · stale-base guard is the engine's").font(.caption2).foregroundStyle(Color(Theme.faint))
            HStack {
                Spacer()
                Button("Cancel") { dismiss() }.keyboardShortcut(.cancelAction)
                Button("Fan out") { app.fanoutNew(prompt: prompt, pearlId: pearlId.nilIfEmpty, candidates: candidates); dismiss() }
                    .keyboardShortcut(.defaultAction).disabled(prompt.isEmpty || candidates.isEmpty)
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

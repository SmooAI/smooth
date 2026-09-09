import AppKit
import SwiftUI

/// ⌘I — what needs you, in order. Cards per attention reason, then finished,
/// then a quiet "working" list. All decisions go back as flow frames.
struct InboxView: View {
    @ObservedObject var store: FlowStore
    @ObservedObject var app: AppController
    /// th-883ce9: the finished session whose Close confirm sheet is up.
    @State private var closing: Session?

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 18) {
                Text("Inbox · what needs you, in order").font(.title3.bold())
                Text("⌘I toggles · every attention behavior is a setting").font(.caption).foregroundStyle(Color(Theme.muted))

                group("NEEDS YOU · \(store.needsYou.count)") {
                    if store.needsYou.isEmpty { quiet("Nothing needs you.") }
                    ForEach(store.needsYou) { s in attentionCard(s) }
                }

                group("FINISHED · \(store.finished.count)") {
                    if store.finished.isEmpty { quiet("Nothing finished yet.") }
                    ForEach(store.finished) { s in finishedCard(s) }
                    ForEach(Array(store.fanOuts.values), id: \.id) { f in fanOutCard(f) }
                }

                group("WORKING · \(store.working.count) · quiet") {
                    ForEach(store.working) { s in
                        HStack {
                            StateDot(session: s)
                            Text(s.label).font(.caption)
                            Spacer()
                            Text(Theme.relative(s.updatedAt)).font(.caption2).foregroundStyle(Color(Theme.faint))
                        }
                    }
                }
                Text("State comes from Claude Code hooks (Stop, Notification, PermissionRequest, PreCompact), not from watching the screen.")
                    .font(.caption2).foregroundStyle(Color(Theme.faint))
            }
            .padding(20)
        }
        .frame(minWidth: 520, minHeight: 400)
        .sheet(item: $closing) { s in
            CloseSessionSheet(session: s, handoff: app.handoffs[s.id]) { closePearl, removeWorktree in
                app.close(s, closePearl: closePearl, removeWorktree: removeWorktree)
            }
        }
    }

    private func group<C: View>(_ title: String, @ViewBuilder _ c: () -> C) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(title).font(.caption.bold()).foregroundStyle(Color(Theme.muted))
            c()
        }
    }

    private func quiet(_ t: String) -> some View { Text(t).font(.caption).foregroundStyle(Color(Theme.faint)) }

    @ViewBuilder
    private func attentionCard(_ s: Session) -> some View {
        let a = s.attention
        card(accent: Theme.amber, id: "inbox.card.\(s.id)") {
            switch a?.reason {
            case .permission:
                title("Permission request", s)
                Text(a?.detail ?? "").font(Theme.mono(.body))
                HStack {
                    Button("Allow") { app.approve(s, .allow) }.keyboardShortcut("a", modifiers: []).accessibilityIdentifier("inbox.allow.\(s.id)")
                    Button("Deny") { app.approve(s, .deny) }.keyboardShortcut("d", modifiers: []).accessibilityIdentifier("inbox.deny.\(s.id)")
                    Button("Allow for session") { app.approve(s, .allowSession) }.accessibilityIdentifier("inbox.allowSession.\(s.id)")
                    Button("Open session") { app.focus(s.id); app.toggleInbox() }
                }
            case .question:
                title("Question", s)
                Text(a?.detail ?? "").font(.body)
                HStack { Button("Open session") { app.focus(s.id); app.toggleInbox() } }
            case .usageLimit:
                title("Usage limit" + (a?.resumeDate.map { " · resumes itself at \(AttentionNotifier.timeFormatter.string(from: $0))" } ?? ""), s)
                Text("Claude Code hit the usage limit. The session resumes on its own when the limit resets. Nothing to do unless you want it sooner.")
                    .font(.caption)
                HStack { Button("Open session") { app.focus(s.id); app.toggleInbox() } }
            case .held:
                title("FYI · idle session held by another process", s)
                Text(a?.detail ?? "owned by pid \(a?.pid.map(String.init) ?? "?")").font(Theme.mono(.caption))
                HStack {
                    Button("Kill and resume here") { app.kill(s, resume: true) }
                    Button("Leave it") { app.markRead(s.id) }
                }
            case .crashed:
                title("Crashed", s)
                Text(a?.detail ?? "gave up after 3 relaunches").font(.caption)
                HStack {
                    Button("Resume") { app.kill(s, resume: true) }
                    Button("Open session") { app.focus(s.id); app.toggleInbox() }
                }
            default:
                title("Needs you", s)
                HStack { Button("Open session") { app.focus(s.id); app.toggleInbox() } }
            }
        }
    }

    private func finishedCard(_ s: Session) -> some View {
        let h = app.handoffs[s.id]
        return card(accent: Theme.ink, id: "inbox.finished.\(s.id)") {
            title(s.label, s)
            if let pr = h?.pr, let n = pr.number { Text("PR #\(n)" + (pr.ci.map { " · CI \($0)" } ?? "")).font(.caption) }
            if let dirty = h?.handoff?.dirty, !dirty.isEmpty { Text("\(dirty.count) dirty files").font(.caption) }
            if s.state == .dead { Text("died · exit \(s.exitCode.map(String.init) ?? "?")").font(.caption).foregroundStyle(.red) }
            HStack {
                Button("Review diff") { app.focus(s.id); app.showTab(.diff); app.toggleInbox() }
                Button("Merge") { app.merge(s) }.disabled(h?.pr?.url == nil)
                Button("Open session") { app.focus(s.id); app.toggleInbox() }
                Spacer()
                // Close = pearl closed + merged worktree gone + row dropped
                // (th-883ce9). Quiet, at the end: the affirmative act is on the sheet.
                Button("Close…") { closing = s }.accessibilityIdentifier("inbox.close.\(s.id)")
            }
            if let r = app.closeRefusals[s.id] { refusal(s, r) }
        }
        .task { await app.loadHandoff(for: s.id) }
    }

    /// The engine refused (dirty or unmerged worktree, nothing touched): say
    /// why in its words, and offer force — the one destructive path, and only
    /// after the reason was read. Amber is for "needs you"; this is not that.
    private func refusal(_ s: Session, _ r: CloseRefusal) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            Text("Not closed: \(r.message)").font(.caption).foregroundStyle(Color(Theme.ink)).accessibilityIdentifier("inbox.close.refusal.\(s.id)")
            HStack {
                Button("Force close") { app.forceClose(s) }.accessibilityIdentifier("inbox.close.force.\(s.id)")
                Button("Keep it") { app.dismissCloseRefusal(s.id) }
            }
        }
        .padding(.top, 2)
    }

    private func fanOutCard(_ f: FanOut) -> some View {
        let cands = (store.fanOutCandidates[f.id] ?? []).compactMap { store.sessions[$0] }
        let done = cands.filter { $0.state == .done }.count
        return card(accent: Theme.teal) {
            Text("Fan-out: \(f.prompt.prefix(60))").font(.headline)
            Text("\(done) of \(cands.count) candidates done").font(.caption)
            HStack { Button("Compare") { app.showFanOut(existing: f.id) } }
        }
    }

    private func title(_ t: String, _ s: Session) -> some View {
        HStack {
            Text(t).font(.headline)
            Spacer()
            Text("\(s.pearlId ?? s.title) · \(Theme.relative(s.updatedAt))").font(.caption).foregroundStyle(Color(Theme.muted))
        }
    }

    private func card<C: View>(accent: NSColor, id: String? = nil, @ViewBuilder _ c: () -> C) -> some View {
        VStack(alignment: .leading, spacing: 8) { c() }
            .accessibilityElement(children: .contain)
            .accessibilityIdentifier(id ?? "inbox.card")
            .padding(12)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(RoundedRectangle(cornerRadius: 8).fill(Color(NSColor.controlBackgroundColor)))
            .overlay(RoundedRectangle(cornerRadius: 8).stroke(Color(accent).opacity(0.5)))
            .controlSize(.small)
    }
}

/// "What will happen" before `flow.close` goes out: each action is named with
/// its target and can be left out; the refusal rule is stated up front so a
/// dirty worktree is never a surprise. Confirm is the default button.
struct CloseSessionSheet: View {
    let session: Session
    let handoff: Handoff?
    let confirm: (_ closePearl: Bool, _ removeWorktree: Bool) -> Void
    @Environment(\.dismiss) private var dismiss
    @State private var closePearl: Bool
    @State private var removeWorktree: Bool

    init(session: Session, handoff: Handoff?, confirm: @escaping (_ closePearl: Bool, _ removeWorktree: Bool) -> Void) {
        self.session = session
        self.handoff = handoff
        self.confirm = confirm
        _closePearl = State(initialValue: session.pearlId != nil)
        _removeWorktree = State(initialValue: Self.hasOwnWorktree(session))
    }

    /// The main checkout is never removed (the engine refuses too); only a
    /// row that lives in its own worktree offers the toggle.
    static func hasOwnWorktree(_ s: Session) -> Bool { !s.worktree.isEmpty && s.worktree != s.project }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Close \(session.label)").font(.headline)
            Text("This finishes the session for good. It leaves the fleet; its scrollback stays until you quit.").font(.caption).foregroundStyle(Color(Theme.muted))
            if let p = session.pearlId {
                Toggle(isOn: $closePearl) { Text("Close pearl \(p)").font(Theme.mono(.body)) }.accessibilityIdentifier("inbox.close.pearl")
            } else {
                Text("No pearl on this session.").font(.caption).foregroundStyle(Color(Theme.faint))
            }
            if Self.hasOwnWorktree(session) {
                Toggle(isOn: $removeWorktree) {
                    VStack(alignment: .leading, spacing: 2) {
                        Text("Remove worktree \(session.worktree)").font(Theme.mono(.body))
                        if let b = session.branch { Text("and delete branch \(b)").font(.caption).foregroundStyle(Color(Theme.muted)) }
                    }
                }.accessibilityIdentifier("inbox.close.worktree")
                Text("Only once the branch is merged and the worktree is clean; otherwise the engine refuses and touches nothing — you can force it from the card.")
                    .font(.caption).foregroundStyle(Color(Theme.muted))
                if let dirty = handoff?.handoff?.dirty, !dirty.isEmpty {
                    Text("\(dirty.count) uncommitted files right now.").font(.caption).foregroundStyle(Color(Theme.ink))
                }
            } else {
                Text("Main checkout — the worktree is kept.").font(.caption).foregroundStyle(Color(Theme.faint))
            }
            HStack {
                Spacer()
                Button("Cancel") { dismiss() }.keyboardShortcut(.cancelAction).accessibilityIdentifier("inbox.close.cancel")
                Button("Close session") { confirm(closePearl, removeWorktree); dismiss() }
                    .keyboardShortcut(.defaultAction).accessibilityIdentifier("inbox.close.confirm")
            }
        }
        .padding(20)
        .frame(width: 460)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("inbox.close.sheet")
    }
}

@MainActor
final class InboxWindowController: NSWindowController {
    init(app: AppController) {
        let w = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 600, height: 560), styleMask: [.titled, .closable, .resizable], backing: .buffered, defer: false)
        w.title = "Inbox"
        w.contentView = NSHostingView(rootView: InboxView(store: app.store, app: app))
        w.center()
        super.init(window: w)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { nil }
}

import AppKit
import SwiftUI

/// ⌘I — what needs you, in order. Cards per attention reason, then finished,
/// then a quiet "working" list. All decisions go back as flow frames.
struct InboxView: View {
    @ObservedObject var store: FlowStore
    @ObservedObject var app: AppController

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
                Text(a?.detail ?? "").font(.body.monospaced())
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
                Text(a?.detail ?? "owned by pid \(a?.pid.map(String.init) ?? "?")").font(.caption.monospaced())
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
            }
        }
        .task { await app.loadHandoff(for: s.id) }
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

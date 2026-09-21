import SwiftUI

/// Right rail: the focused session's pearl + handoff packet
/// (`GET /api/flow/sessions/{id}/handoff`).
struct PearlRail: View {
    @ObservedObject var store: FlowStore
    @ObservedObject var app: AppController

    var body: some View {
        ScrollView {
            if let s = store.focused {
                content(s, app.handoffs[s.id])
            } else {
                Text("No session focused").foregroundStyle(Color(Theme.muted)).padding()
            }
        }
        .frame(minWidth: 240)
        .task(id: store.focusedId) { if let id = store.focusedId { await app.loadHandoff(for: id) } }
    }

    @ViewBuilder
    private func content(_ s: Session, _ h: Handoff?) -> some View {
        VStack(alignment: .leading, spacing: 14) {
            VStack(alignment: .leading, spacing: 4) {
                Text("PEARL · \(s.pearlId ?? "none")").font(.caption).foregroundStyle(Color(Theme.muted))
                Text(h?.pearl?.title ?? s.title).font(.headline)
                HStack(spacing: 6) {
                    if let st = h?.pearl?.status { chip(st) }
                    if let p = h?.pearl?.priority { chip("P\(p)") }
                    ForEach(h?.pearl?.labels ?? [], id: \.self) { chip($0) }
                }
            }

            section("HANDOFF PACKET") {
                mono("worktree", h?.handoff?.worktree ?? s.worktree)
                mono("branch", h?.handoff?.branch ?? s.branch ?? "")
                if let head = h?.handoff?.head {
                    mono("HEAD", head + ((h?.handoff?.dirty?.count).map { $0 > 0 ? " (+\($0) dirty)" : "" } ?? ""))
                }
                mono("session", "\(s.kind) \(s.agentSessionId?.prefix(4) ?? "")" + (s.agentSessionId != nil ? "… (resumable)" : ""))
                if let next = h?.handoff?.next { mono("next", next) }
            }

            if let cps = h?.checkpoints, !cps.isEmpty {
                section("CHECKPOINTS") {
                    ForEach(cps) { c in
                        HStack(alignment: .top, spacing: 6) {
                            Text(String(c.at.suffix(8).prefix(5))).font(Theme.mono(.caption)).foregroundStyle(Color(Theme.faint))
                            Text(c.note + (c.auto == true ? " (auto)" : "")).font(.caption)
                        }
                    }
                }
            }

            if let blocks = h?.blocks, !blocks.isEmpty {
                section("BLOCKS") { Text(blocks.joined(separator: " · ")).font(Theme.mono(.caption)) }
            }

            if let pr = h?.pr, let n = pr.number {
                section("PR") {
                    HStack {
                        Text("#\(n)").font(Theme.mono(.caption))
                        if let ci = pr.ci { chip("CI \(ci)") }
                        if let u = pr.url, let url = URL(string: u) { Link("open", destination: url).font(.caption) }
                    }
                }
            }

            HStack(spacing: 8) {
                Button("Checkpoint") { app.runTh(["pearls", "checkpoint", s.pearlId ?? ""], in: s.worktree) }.disabled(s.pearlId == nil)
                Button("Hand off") { app.runTh(["pearls", "prime", s.pearlId ?? ""], in: s.worktree) }.disabled(s.pearlId == nil)
                Button("Close") { app.runTh(["pearls", "close", s.pearlId ?? ""], in: s.worktree) }.disabled(s.pearlId == nil)
            }.controlSize(.small)

            if let msg = app.thOutput, !msg.isEmpty {
                Text(msg).font(Theme.mono(.caption)).foregroundStyle(Color(Theme.muted)).textSelection(.enabled)
            }
        }
        .padding(14)
    }

    private func section<C: View>(_ title: String, @ViewBuilder _ c: () -> C) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(title).font(.caption).foregroundStyle(Color(Theme.muted))
            c()
        }
    }

    private func mono(_ k: String, _ v: String) -> some View {
        HStack(alignment: .top, spacing: 6) {
            Text(k).font(.caption).foregroundStyle(Color(Theme.faint)).frame(width: 60, alignment: .trailing)
            Text(v).font(Theme.mono(.caption)).textSelection(.enabled)
        }
    }

    private func chip(_ t: String) -> some View {
        Text(t).font(.caption2).padding(.horizontal, 6).padding(.vertical, 2)
            .background(Color(Theme.faint).opacity(0.2)).clipShape(Capsule())
    }
}

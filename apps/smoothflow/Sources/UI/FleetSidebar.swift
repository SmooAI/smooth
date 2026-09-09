import SwiftUI

/// Left rail: every session grouped by project, state dot + unread badge.
struct FleetSidebar: View {
    @ObservedObject var store: FlowStore
    @ObservedObject var app: AppController

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            header
            List(selection: Binding(get: { store.focusedId }, set: { if let id = $0 { app.focus(id) } })) {
                ForEach(store.grouped, id: \.project) { group in
                    Section(group.project) {
                        ForEach(group.sessions) { s in
                            row(s).tag(s.id)
                        }
                    }
                }
            }
            .listStyle(.sidebar)
            footer
        }
        .frame(minWidth: 220)
    }

    private var header: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack {
                Text("FLEET · \(store.order.count) sessions").font(.caption).foregroundStyle(Color(Theme.muted)).accessibilityIdentifier("sidebar.header")
                Spacer()
                if store.counts.needsYou > 0 {
                    Button { app.toggleInbox() } label: {
                        Text("Needs you · \(store.counts.needsYou)").font(.caption.bold())
                            .padding(.horizontal, 8).padding(.vertical, 3)
                            .background(Color(Theme.amberWash)).foregroundStyle(Color(Theme.amber)).clipShape(Capsule())
                    }.buttonStyle(.plain).accessibilityIdentifier("sidebar.needsYou")
                }
            }
            HStack(spacing: 8) {
                Button("+ New session") { app.showNewSession() }.keyboardShortcut("n")
                Button("Fan out") { app.showFanOut() }
            }.controlSize(.small)
        }
        .padding(12)
    }

    private func row(_ s: Session) -> some View {
        HStack(spacing: 8) {
            StateDot(session: s)
            VStack(alignment: .leading, spacing: 1) {
                Text(s.label).lineLimit(1).accessibilityIdentifier("sidebar.title.\(s.id)")
                Text(Theme.stateLabel(s) + (s.state == .done && app.handoffs[s.id]?.pr?.number != nil ? " · PR #\(app.handoffs[s.id]!.pr!.number!)" : ""))
                    .font(.caption).foregroundStyle(s.needsYou ? Color(Theme.amber) : Color(Theme.muted))
                    .accessibilityIdentifier("sidebar.state.\(s.id)")
            }
            Spacer()
            if s.unread { UnreadBadge() }
        }
        .padding(.vertical, 2)
        .accessibilityIdentifier("sidebar.session.\(s.id)")
    }

    private var footer: some View {
        let c = store.counts
        return VStack(alignment: .leading, spacing: 3) {
            HStack(spacing: 6) {
                Circle().fill(store.connection.isConnected ? Color(Theme.teal) : Color(.systemRed)).frame(width: 7, height: 7)
                Text(connectionText).font(.caption).accessibilityIdentifier("sidebar.connection")
            }
            Text("\(c.working) working · \(c.needsYou) need you · \(c.done) done · \(c.idle) idle").font(.caption2).foregroundStyle(Color(Theme.muted))
        }
        .padding(12)
    }

    private var connectionText: String {
        switch store.connection {
        case .connected(let d): "daemon connected · \(d.machineLabel.isEmpty ? d.version : d.machineLabel)"
        case .connecting: "connecting…"
        case .disconnected(let r): "disconnected" + (r.map { " · \($0)" } ?? "")
        }
    }
}

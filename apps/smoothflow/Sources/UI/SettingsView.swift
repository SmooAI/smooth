import AppKit
import SwiftUI

/// ⌘, — Permissions (live TCC status + asks), Attention (per-reason toggles),
/// Harnesses (order + hide, th-0f6126), Daemon (child vs LaunchAgent, address
/// override).
struct SettingsView: View {
    @ObservedObject var app: AppController
    @ObservedObject var permissions: Permissions
    @ObservedObject var daemon: DaemonManager
    @State private var notify = NotifySettings.load()
    @State private var addr = UserDefaults.standard.string(forKey: DaemonAddress.defaultsKey) ?? ""
    @State private var binary = UserDefaults.standard.string(forKey: DaemonAddress.binaryDefaultsKey) ?? ""
    @State private var mode: DaemonManager.Mode = .child

    var body: some View {
        TabView {
            PermissionsPane(permissions: permissions, daemon: daemon).accessibilityIdentifier("settings.pane.permissions").tabItem { Text("Permissions") }
            attention.accessibilityIdentifier("settings.pane.attention").tabItem { Text("Attention") }
            HarnessesPane(app: app).accessibilityIdentifier("settings.pane.harnesses").tabItem { Text("Harnesses") }
            daemonPane.accessibilityIdentifier("settings.pane.daemon").tabItem { Text("Daemon") }
        }
        .padding(16)
        .frame(width: 560, height: 440)
        .onAppear { mode = daemon.mode; permissions.refresh() }
    }

    private var attention: some View {
        Form {
            Text("Every attention behavior is a setting. A notification carries the session id; clicking it focuses the session.")
                .font(.caption).foregroundStyle(Color(Theme.muted))
            Toggle("Permission requests", isOn: $notify.permission)
            Toggle("Questions", isOn: $notify.question)
            Toggle("Usage limits (with resume time)", isOn: $notify.usageLimit)
            Toggle("Crashes", isOn: $notify.crashed)
            Toggle("Sessions held by another process", isOn: $notify.held)
            Toggle("Finished sessions", isOn: $notify.finished)
            Toggle("Play a sound", isOn: $notify.sound)
        }
        .onChange(of: notify) { _, n in n.save(); app.notifySettings = n }
    }

    private var daemonPane: some View {
        Form {
            Picker("Launch smooth-daemon as", selection: $mode) {
                Text("Child of this app (default)").tag(DaemonManager.Mode.child)
                Text("LaunchAgent inside the bundle (SMAppService)").tag(DaemonManager.Mode.launchAgent).disabled(!daemon.launchAgentAvailable)
            }
            .onChange(of: mode) { _, m in daemon.mode = m }
            if !daemon.launchAgentAvailable {
                Text("LaunchAgent mode needs the daemon bundled in SmoothFlow.app (release builds).").font(.caption).foregroundStyle(Color(Theme.muted))
            }
            LabeledContent("Status", value: daemon.status).accessibilityIdentifier("settings.daemon.status")
            LabeledContent("Binary", value: daemon.binary ?? "not found")
            LabeledContent("Endpoint", value: daemon.endpoint?.description ?? "—")
            TextField("Connect to an external daemon instead (host:port) — dev/mock only", text: $addr)
                .font(.body.monospaced())
                .onSubmit { UserDefaults.standard.set(addr, forKey: DaemonAddress.defaultsKey); app.restartConnection() }
            TextField("Launch this smooth-daemon binary instead of the bundled one — dev only", text: $binary)
                .font(.body.monospaced())
                .onSubmit { UserDefaults.standard.set(binary, forKey: DaemonAddress.binaryDefaultsKey); app.restartConnection() }
            Text("Never rely on a daemon started from a terminal: macOS attributes its TCC prompts to that terminal and denies them silently.")
                .font(.caption).foregroundStyle(Color(Theme.muted))
            Button("Restart daemon") { app.restartConnection() }
        }
    }
}

/// Settings ▸ Harnesses: the engine's full list (hidden ones too), reordered
/// with ▲/▼ and hidden with a toggle — every change is a PUT to the engine,
/// and the engine's `flow.harnesses` then updates every picker and phone.
struct HarnessesPane: View {
    @ObservedObject var app: AppController

    private var order: [String] { app.allHarnesses.map(\.name) }
    private var hidden: [String] { app.allHarnesses.filter(\.hidden).map(\.name) }

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("The order here is the order in every picker — New session, fan-out candidates, the phones. Hidden harnesses keep their manifest and come back with one toggle. `th harness add` installs a new one.")
                .font(.caption).foregroundStyle(Color(Theme.muted))
            if app.allHarnesses.isEmpty {
                Text("No harness list yet — connect to the engine.").font(.caption).foregroundStyle(Color(Theme.faint))
            }
            ForEach(Array(app.allHarnesses.enumerated()), id: \.element.id) { i, h in
                HStack(spacing: 8) {
                    Text(h.installed ? "●" : "○").foregroundStyle(h.installed ? Color(Theme.teal) : Color(Theme.faint))
                    VStack(alignment: .leading, spacing: 1) {
                        HStack(spacing: 6) {
                            Text(h.displayName).font(.headline).foregroundStyle(h.hidden ? Color(Theme.muted) : .primary)
                            Text(h.stateSource).font(.caption.monospaced()).foregroundStyle(Color(Theme.faint))
                            Text(h.origin).font(.caption2).foregroundStyle(Color(Theme.faint))
                        }
                        Text(h.binaryPath ?? h.reason ?? "").font(.caption.monospaced()).foregroundStyle(Color(Theme.muted)).lineLimit(1)
                    }
                    Spacer()
                    Button("▲") { move(i, -1) }.disabled(i == 0)
                    Button("▼") { move(i, 1) }.disabled(i == app.allHarnesses.count - 1)
                    Toggle("Shown", isOn: Binding(get: { !h.hidden }, set: { _ in toggle(h.name) })).toggleStyle(.switch).labelsHidden()
                }
                .controlSize(.small)
            }
            Spacer()
        }
        .task { await app.loadAllHarnesses() }
    }

    private func move(_ i: Int, _ delta: Int) {
        let next = HarnessOrdering.moved(order, at: i, by: delta)
        guard next != order else { return }
        Task { await app.setHarnessPrefs(order: next) }
    }

    private func toggle(_ name: String) {
        Task { await app.setHarnessPrefs(hidden: HarnessOrdering.toggled(hidden, name)) }
    }
}

struct PermissionsPane: View {
    @ObservedObject var permissions: Permissions
    @ObservedObject var daemon: DaemonManager
    var compact = false

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            ForEach(PermissionKind.allCases) { kind in
                let st = permissions.status[kind] ?? .unknown
                HStack(alignment: .top, spacing: 10) {
                    Text(st.symbol).foregroundStyle(color(st)).font(.title3)
                    VStack(alignment: .leading, spacing: 2) {
                        HStack {
                            Text(kind.title).font(.headline)
                            Text(st.rawValue).font(.caption).foregroundStyle(Color(Theme.muted))
                        }
                        if !compact { Text(kind.why).font(.caption).foregroundStyle(Color(Theme.muted)) }
                    }
                    Spacer()
                    if st != .granted {
                        Button(kind == .fullDiskAccess ? "Open System Settings" : "Grant…") { permissions.request(kind) }.controlSize(.small)
                    }
                    if st == .denied || kind == .fullDiskAccess {
                        Button("Settings") { permissions.openSettings(kind) }.controlSize(.small)
                    }
                }
            }
            Divider()
            HStack {
                Button("Re-check") { permissions.refresh() }
                Button("Probe Calendar from a child process") { permissions.probeChildCalendar(daemonBinary: daemon.resolveBinary()) }
            }.controlSize(.small)
            Text(permissions.childCalendarReport).font(.caption.monospaced()).foregroundStyle(Color(Theme.muted))
            if permissions.status[.fullDiskAccess] != .granted {
                Text("Full Disk Access: in the pane, click +, choose SmoothFlow.app, and toggle it on. Re-checked when the app activates.")
                    .font(.caption).foregroundStyle(Color(Theme.amber))
            }
        }
    }

    private func color(_ s: PermissionStatus) -> Color {
        switch s {
        case .granted: Color(Theme.teal)
        case .denied: Color(.systemRed)
        case .notDetermined, .unknown: Color(Theme.faint)
        }
    }
}

/// First run: what the app is, and the asks — all from the main executable.
struct OnboardingView: View {
    @ObservedObject var permissions: Permissions
    @ObservedObject var daemon: DaemonManager
    var dismiss: () -> Void = {}

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            Text("Welcome to SmoothFlow").font(.title2.bold())
            Text("A fleet console for your agents. SmoothFlow runs the Smooth engine itself so the macOS permissions it needs — Notifications, Calendar, Full Disk Access — belong to this app and are inherited by every agent it starts.")
                .font(.body)
            PermissionsPane(permissions: permissions, daemon: daemon, compact: true)
            HStack {
                Spacer()
                Button("Later") { dismiss() }.keyboardShortcut(.cancelAction)
                Button("Done") { dismiss() }.keyboardShortcut(.defaultAction)
            }
        }
        .padding(24)
        .frame(width: 560)
        .onAppear { permissions.refresh() }
    }
}

@MainActor
final class SettingsWindowController: NSWindowController {
    init(app: AppController) {
        let w = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 560, height: 460), styleMask: [.titled, .closable], backing: .buffered, defer: false)
        w.title = "SmoothFlow Settings"
        w.contentView = NSHostingView(rootView: SettingsView(app: app, permissions: app.permissions, daemon: app.daemon))
        w.center()
        super.init(window: w)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { nil }
}

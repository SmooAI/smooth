import AppKit
import SwiftUI

/// ⌘, — Permissions (live TCC status + asks), Attention (per-reason toggles),
/// Harnesses (order + hide, th-0f6126), Terminal (the bundled Nerd Font, size,
/// ligatures — th-bcd819), Phones (QR pairing for end-to-end encrypted relay
/// frames, th-d98fde), Daemon (child vs LaunchAgent, address override).
struct SettingsView: View {
    @ObservedObject var app: AppController
    @ObservedObject var permissions: Permissions
    @ObservedObject var daemon: DaemonManager
    @State private var notify = NotifySettings.load()
    @State private var addr = UserDefaults.standard.string(forKey: DaemonAddress.defaultsKey) ?? ""
    @State private var binary = UserDefaults.standard.string(forKey: DaemonAddress.binaryDefaultsKey) ?? ""
    @State private var mode: DaemonManager.Mode = .child

    var body: some View {
        // Each pane is an AX container (`children: .contain`) so its identifier
        // names the pane and the buttons inside keep their own — without it the
        // macOS 26 runner stamped `settings.pane.<x>` on EVERY child and
        // `settings.phones.pair` never existed (th-2ecc1c).
        TabView {
            PermissionsPane(permissions: permissions, daemon: daemon).accessibilityElement(children: .contain).accessibilityIdentifier("settings.pane.permissions").tabItem { Text("Permissions") }
            attention.accessibilityElement(children: .contain).accessibilityIdentifier("settings.pane.attention").tabItem { Text("Attention") }
            HarnessesPane(app: app).accessibilityElement(children: .contain).accessibilityIdentifier("settings.pane.harnesses").tabItem { Text("Harnesses") }
            TerminalPane().accessibilityElement(children: .contain).accessibilityIdentifier("settings.pane.terminal").tabItem { Text("Terminal") }
            PhonesPane(app: app).accessibilityElement(children: .contain).accessibilityIdentifier("settings.pane.phones").tabItem { Text("Phones") }
            daemonPane.accessibilityElement(children: .contain).accessibilityIdentifier("settings.pane.daemon").tabItem { Text("Daemon") }
        }
        .classicTabs()
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
                .font(Theme.mono(.body))
                .onSubmit { UserDefaults.standard.set(addr, forKey: DaemonAddress.defaultsKey); app.restartConnection() }
            TextField("Launch this smooth-daemon binary instead of the bundled one — dev only", text: $binary)
                .font(Theme.mono(.body))
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
                            Text(h.stateSource).font(Theme.mono(.caption)).foregroundStyle(Color(Theme.faint))
                            Text(h.origin).font(.caption2).foregroundStyle(Color(Theme.faint))
                        }
                        Text(h.binaryPath ?? h.reason ?? "").font(Theme.mono(.caption)).foregroundStyle(Color(Theme.muted)).lineLimit(1)
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


/// Settings ▸ Phones: pair a phone (QR → the phone derives a key only the two
/// of them hold; the relay carries ciphertext), see who is paired and when
/// they were last here, revoke. Presence is a glyph, not a color: ● here,
/// ◐ today, ○ away. Teal marks the engine's own presence; amber is not used —
/// nothing here needs you.
/// Settings ▸ Terminal (th-bcd819): which font the panes use. Every change is
/// saved and pushed to the open surfaces at once (`GhosttyRuntime.reloadConfig`).
/// Precedence lives in `TerminalFont`: a choice here → the user's Ghostty
/// config → the bundled JetBrainsMono Nerd Font.
struct TerminalPane: View {
    @State private var settings = TerminalSettings.load()
    @State private var families = TerminalFont.availableFamilies()
    private let userKeys = TerminalFont.readUserConfigKeys()

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Panes use JetBrainsMono Nerd Font, shipped with the app, unless your Ghostty config or a choice here says otherwise. The same face is used for the app's own monospace text.")
                .font(.caption).foregroundStyle(Color(Theme.muted))
            Picker("Font", selection: family) {
                Text(userKeys.contains("font-family") ? "Ghostty config (font-family)" : "Bundled · \(TerminalFont.bundledFamily)").tag("")
                ForEach(families, id: \.self) { Text($0).tag($0) }
            }
            .accessibilityIdentifier("settings.terminal.family")
            HStack {
                Stepper(value: size, in: TerminalFont.sizeRange, step: 1) {
                    Text("Size  \(Int(settings.size ?? TerminalFont.defaultSize)) pt")
                }
                .accessibilityIdentifier("settings.terminal.size")
                Button("Default size") { settings.size = nil; apply() }.disabled(settings.size == nil)
            }
            Toggle("Ligatures", isOn: ligatures).accessibilityIdentifier("settings.terminal.ligatures")
            if userKeys.contains("font-family"), settings.family == nil {
                Text("Your Ghostty config sets font-family, so the panes follow it. Pick a font above to override just SmoothFlow.")
                    .font(.caption).foregroundStyle(Color(Theme.muted))
            }
            VStack(alignment: .leading, spacing: 2) {
                Text("SAMPLE").font(.caption.bold()).foregroundStyle(Color(Theme.muted))
                Text("$ th flow ls   \u{e0a0} main   \u{f00c} 84 tests   0O o0 1lI| -> => != ...")
                    .font(Theme.mono(.body)).textSelection(.enabled).accessibilityIdentifier("settings.terminal.sample")
            }
            Spacer()
        }
        .padding(4)
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    private var family: Binding<String> {
        Binding(get: { settings.family ?? "" }, set: { settings.family = $0.isEmpty ? nil : $0; apply() })
    }

    private var size: Binding<Double> {
        Binding(get: { settings.size ?? TerminalFont.defaultSize }, set: { settings.size = $0; apply() })
    }

    private var ligatures: Binding<Bool> {
        Binding(get: { settings.ligatures }, set: { settings.ligatures = $0; apply() })
    }

    private func apply() {
        settings.save()
        GhosttyRuntime.shared.reloadConfig()
    }
}

struct PhonesPane: View {
    @ObservedObject var app: AppController

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text("A paired phone talks to this Mac through the Smoo Relay in frames only the two of them can read. Scan the QR with the SmoothFlow app (Connect ▸ Pair a Mac) or the Camera app. Re-pairing a phone rotates its key.")
                .font(.caption).foregroundStyle(Color(Theme.muted))
            if let list = app.pairedPhones, !list.relayEnabled {
                Text("The relay is off for this engine (SMOOTH_RELAY=0) — phones cannot reach it.").font(.caption).foregroundStyle(Color(Theme.amber))
            }
            if let pending = app.pendingPairing {
                HStack(alignment: .top, spacing: 14) {
                    if let img = PairingQR.image(for: pending.url) {
                        Image(nsImage: img).interpolation(.none).resizable().frame(width: 168, height: 168)
                            .accessibilityIdentifier("settings.phones.qr").accessibilityLabel("Pairing QR")
                    }
                    VStack(alignment: .leading, spacing: 6) {
                        HStack(spacing: 6) {
                            ProgressView().controlSize(.small)
                            Text("Waiting for the scan…").font(.headline)
                        }
                        Text("Pairs with \(pending.label) · \(pending.device)").font(.caption).foregroundStyle(Color(Theme.muted))
                        Text("Code \(pending.code)").font(Theme.mono(.caption)).foregroundStyle(Color(Theme.muted)).textSelection(.enabled)
                        if let exp = pending.expiresAt { Text("Expires \(Theme.clock(exp))").font(.caption).foregroundStyle(Color(Theme.faint)) }
                        HStack {
                            Button("Copy link") {
                                NSPasteboard.general.clearContents()
                                NSPasteboard.general.setString(pending.url, forType: .string)
                            }
                            Button("Cancel") { app.cancelPairing() }
                        }.controlSize(.small)
                    }
                }
            } else {
                Button("Pair a phone…") { Task { await app.beginPairing() } }.accessibilityIdentifier("settings.phones.pair")
            }
            if let msg = app.pairingMessage {
                Text(msg).font(.caption).foregroundStyle(msg.hasPrefix("Paired") ? Color(Theme.teal) : Color(Theme.muted)).accessibilityIdentifier("settings.phones.message")
            }
            Divider()
            let phones = app.pairedPhones?.pairings ?? []
            if phones.isEmpty {
                Text("No paired phones.").font(.caption).foregroundStyle(Color(Theme.faint))
            }
            ForEach(phones) { p in
                let presence = PhonePresence.of(lastSeen: p.lastSeenAt)
                HStack(spacing: 8) {
                    Text(presence.glyph).foregroundStyle(presence == .here ? Color(Theme.teal) : Color(Theme.faint)).font(.title3)
                    VStack(alignment: .leading, spacing: 1) {
                        HStack(spacing: 6) {
                            Text(p.label).font(.headline)
                            Text(p.platform).font(Theme.mono(.caption)).foregroundStyle(Color(Theme.faint))
                        }
                        Text("\(p.device) · paired \(Theme.relative(p.createdAt)) · \(p.lastSeenAt.map { "seen " + Theme.relative($0) } ?? "never seen")")
                            .font(Theme.mono(.caption)).foregroundStyle(Color(Theme.muted)).lineLimit(1)
                    }
                    Spacer()
                    Button("Revoke") { Task { await app.revokePairing(p.device) } }.controlSize(.small).accessibilityIdentifier("settings.phones.revoke.\(p.device)")
                }
            }
            Spacer()
        }
        .task { await app.loadPairings() }
        .onDisappear { app.cancelPairing() }
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
            Text(permissions.childCalendarReport).font(Theme.mono(.caption)).foregroundStyle(Color(Theme.muted))
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

extension View {
    /// The classic segmented tab strip. Under the macOS 26 SDK a plain `TabView`
    /// becomes a window-toolbar tab bar, and in a titled window that has no
    /// toolbar every tab collapses into a `»` overflow menu — the CI runner
    /// (Xcode 26.6) showed an empty toolbar and `radioButtons["Daemon"]` never
    /// existed (th-2ecc1c). Grouped tabs are radio buttons on every macOS.
    @ViewBuilder
    func classicTabs() -> some View {
        if #available(macOS 15, *) { tabViewStyle(.grouped) } else { self }
    }
}

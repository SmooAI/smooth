import AppKit
import EventKit
import Foundation
import UserNotifications

enum PermissionKind: String, CaseIterable, Identifiable {
    case notifications, calendar, reminders, fullDiskAccess, automation
    var id: String { rawValue }

    var title: String {
        switch self {
        case .notifications: "Notifications"
        case .calendar: "Calendar"
        case .reminders: "Reminders"
        case .fullDiskAccess: "Full Disk Access"
        case .automation: "Automation (Messages)"
        }
    }

    var why: String {
        switch self {
        case .notifications: "Know when an agent needs you without watching the window."
        case .calendar: "Agents can read and schedule around your calendar (the daemon shells `ical`)."
        case .reminders: "Agents can read and add reminders."
        case .fullDiskAccess: "Agents can read Mail, Safari and other protected folders. No prompt exists — grant it in System Settings."
        case .automation: "The Messages tool drives Messages.app over Apple Events."
        }
    }

    /// `x-apple.systempreferences:` deep link for the pane.
    var settingsURL: URL {
        let anchor: String
        switch self {
        case .notifications: anchor = "com.apple.Notifications-Settings.extension"
        case .calendar: anchor = "com.apple.preference.security?Privacy_Calendars"
        case .reminders: anchor = "com.apple.preference.security?Privacy_Reminders"
        case .fullDiskAccess: anchor = "com.apple.preference.security?Privacy_AllFiles"
        case .automation: anchor = "com.apple.preference.security?Privacy_Automation"
        }
        return URL(string: "x-apple.systempreferences:\(anchor)")!
    }
}

enum PermissionStatus: String {
    case granted, denied, notDetermined, unknown
    var symbol: String {
        switch self {
        case .granted: "●"
        case .denied: "●"
        case .notDetermined: "○"
        case .unknown: "◌"
        }
    }
}

/// Live TCC status + the asks. Every ask runs from the app's MAIN executable —
/// that is the whole reason the daemon is our child and not a terminal's.
@MainActor
final class Permissions: ObservableObject {
    @Published private(set) var status: [PermissionKind: PermissionStatus] = [:]
    /// What a CHILD process sees for Calendar (proves inheritance).
    @Published private(set) var childCalendarReport: String = "not checked"

    private let eventStore = EKEventStore()

    func refresh() {
        status[.calendar] = Self.map(EKEventStore.authorizationStatus(for: .event))
        status[.reminders] = Self.map(EKEventStore.authorizationStatus(for: .reminder))
        status[.fullDiskAccess] = Self.fullDiskAccessProbe()
        status[.automation] = Self.automationProbe(bundleId: "com.apple.MobileSMS")
        UNUserNotificationCenter.current().getNotificationSettings { settings in
            let s: PermissionStatus = switch settings.authorizationStatus {
            case .authorized, .provisional, .ephemeral: .granted
            case .denied: .denied
            case .notDetermined: .notDetermined
            @unknown default: .unknown
            }
            Task { @MainActor in self.status[.notifications] = s }
        }
    }

    func request(_ kind: PermissionKind) {
        switch kind {
        case .notifications:
            UNUserNotificationCenter.current().requestAuthorization(options: [.alert, .sound, .badge]) { _, _ in
                Task { @MainActor in self.refresh() }
            }
        case .calendar:
            eventStore.requestFullAccessToEvents { granted, error in
                Task { @MainActor in
                    self.childCalendarReport = "calendar request → granted=\(granted)" + (error.map { " error: \($0)" } ?? "")
                    self.refresh()
                }
            }
        case .reminders:
            eventStore.requestFullAccessToReminders { granted, error in
                Task { @MainActor in
                    self.childCalendarReport = "reminders request → granted=\(granted)" + (error.map { " error: \($0)" } ?? "")
                    self.refresh()
                }
            }
        case .fullDiskAccess:
            // No API to prompt. Open the pane; `refresh()` re-probes on activate.
            NSWorkspace.shared.open(kind.settingsURL)
        case .automation:
            // The first real Apple Event prompts. Send a harmless one.
            let script = NSAppleScript(source: "tell application \"Messages\" to get name")
            var err: NSDictionary?
            script?.executeAndReturnError(&err)
            refresh()
        }
    }

    func openSettings(_ kind: PermissionKind) { NSWorkspace.shared.open(kind.settingsURL) }

    /// Run the daemon's own calendar probe as a child and report what it saw.
    /// A child cannot ASK (macOS only prompts an app's main executable) but it
    /// inherits the app's grant — this is how we prove that empirically.
    func probeChildCalendar(daemonBinary: String?) {
        guard let daemonBinary else { childCalendarReport = "no smooth-daemon binary to probe with"; return }
        let p = Process()
        p.executableURL = URL(fileURLWithPath: daemonBinary)
        p.arguments = ["tcc", "calendar"]
        let pipe = Pipe()
        p.standardOutput = pipe
        p.standardError = pipe
        do {
            try p.run()
            p.waitUntilExit()
            let out = String(data: pipe.fileHandleForReading.readDataToEndOfFile(), encoding: .utf8) ?? ""
            childCalendarReport = "child `smooth-daemon tcc calendar` → " + out.trimmingCharacters(in: .whitespacesAndNewlines)
        } catch {
            childCalendarReport = "probe failed: \(error.localizedDescription)"
        }
    }

    // MARK: probes (pure; testable)

    static func map(_ s: EKAuthorizationStatus) -> PermissionStatus {
        switch s {
        case .fullAccess, .authorized: .granted
        case .writeOnly: .granted
        case .denied, .restricted: .denied
        case .notDetermined: .notDetermined
        @unknown default: .unknown
        }
    }

    /// FDA has no query API. Reading a TCC-protected file either works
    /// (granted) or fails with EPERM (denied). Files that do not exist prove
    /// nothing, so we try several.
    static func fullDiskAccessProbe(home: URL = FileManager.default.homeDirectoryForCurrentUser) -> PermissionStatus {
        let probes = ["Library/Safari/Bookmarks.plist", "Library/Mail", "Library/Messages/chat.db", "Library/Application Support/com.apple.TCC/TCC.db"]
        for rel in probes {
            let path = home.appendingPathComponent(rel).path
            var isDir: ObjCBool = false
            guard FileManager.default.fileExists(atPath: path, isDirectory: &isDir) else { continue }
            if isDir.boolValue {
                if (try? FileManager.default.contentsOfDirectory(atPath: path)) != nil { return .granted }
                if errno == EPERM { return .denied }
                return .denied
            }
            if FileManager.default.isReadableFile(atPath: path), let h = FileHandle(forReadingAtPath: path) {
                try? h.close()
                return .granted
            }
            return .denied
        }
        return .unknown
    }

    /// `AEDeterminePermissionToAutomateTarget` with askUserIfNeeded=false does
    /// not prompt. -600 (procNotFound) when the target is not running.
    static func automationProbe(bundleId: String) -> PermissionStatus {
        let target = NSAppleEventDescriptor(bundleIdentifier: bundleId)
        guard let ptr = target.aeDesc else { return .unknown }
        var desc = ptr.pointee
        let err = AEDeterminePermissionToAutomateTarget(&desc, typeWildCard, typeWildCard, false)
        switch Int(err) {
        case 0: return .granted
        case -1743: return .denied
        case -1744: return .notDetermined
        default: return .unknown
        }
    }
}

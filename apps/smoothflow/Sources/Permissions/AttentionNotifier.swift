import Foundation
import UserNotifications

/// Which attention reasons post a notification. Every attention behavior is a
/// setting (Settings ▸ Attention); defaults are all on.
struct NotifySettings: Equatable {
    var permission = true
    var question = true
    var usageLimit = true
    var crashed = true
    var held = true
    var finished = true
    var sound = true

    static let keys: [(String, WritableKeyPath<NotifySettings, Bool>)] = [
        ("notify.permission", \.permission), ("notify.question", \.question), ("notify.usageLimit", \.usageLimit),
        ("notify.crashed", \.crashed), ("notify.held", \.held), ("notify.finished", \.finished), ("notify.sound", \.sound),
    ]

    static func load(_ d: UserDefaults = .standard) -> NotifySettings {
        var s = NotifySettings()
        for (k, kp) in keys where d.object(forKey: k) != nil { s[keyPath: kp] = d.bool(forKey: k) }
        return s
    }

    func save(_ d: UserDefaults = .standard) {
        for (k, kp) in Self.keys { d.set(self[keyPath: kp], forKey: k) }
    }
}

/// A notification the shell wants to post — plain data so the mapping is testable.
struct AttentionNotification: Equatable {
    var sessionId: String
    var title: String
    var body: String
    var category: String
}

enum AttentionNotifier {
    static let categoryPermission = "smoothflow.permission"
    static let categoryGeneric = "smoothflow.attention"
    static let sessionKey = "sessionId"
    static let requestKey = "requestId"

    /// attention → notification (nil = setting off, or nothing to say).
    static func notification(for session: Session, settings: NotifySettings) -> AttentionNotification? {
        let name = session.pearlId ?? session.title
        if session.state == .done || session.state == .dead {
            guard settings.finished else { return nil }
            let how = session.state == .done ? "finished" : "died (exit \(session.exitCode.map(String.init) ?? "?"))"
            return AttentionNotification(sessionId: session.id, title: "\(name) \(how)", body: session.title, category: categoryGeneric)
        }
        guard let a = session.attention else { return nil }
        switch a.reason {
        case .permission:
            guard settings.permission else { return nil }
            return AttentionNotification(sessionId: session.id, title: "\(name) needs approval", body: a.detail ?? "Permission request", category: categoryPermission)
        case .question:
            guard settings.question else { return nil }
            return AttentionNotification(sessionId: session.id, title: "\(name) has a question", body: a.detail ?? "", category: categoryGeneric)
        case .usageLimit:
            guard settings.usageLimit else { return nil }
            let when = a.resumeDate.map { " · resumes \(Self.timeFormatter.string(from: $0))" } ?? ""
            return AttentionNotification(sessionId: session.id, title: "\(name) hit the usage limit", body: "Resumes on its own\(when)", category: categoryGeneric)
        case .crashed:
            guard settings.crashed else { return nil }
            return AttentionNotification(sessionId: session.id, title: "\(name) crashed", body: a.detail ?? "Relaunch gave up after 3 tries", category: categoryGeneric)
        case .held:
            guard settings.held else { return nil }
            return AttentionNotification(sessionId: session.id, title: "\(name) is held by another process", body: a.detail ?? "pid \(a.pid.map(String.init) ?? "?")", category: categoryGeneric)
        case .unknown:
            return nil
        }
    }

    static let timeFormatter: DateFormatter = {
        let f = DateFormatter(); f.dateStyle = .none; f.timeStyle = .short; return f
    }()

    static func registerCategories() {
        let allow = UNNotificationAction(identifier: "allow", title: "Allow", options: [])
        let deny = UNNotificationAction(identifier: "deny", title: "Deny", options: [.destructive])
        let permission = UNNotificationCategory(identifier: categoryPermission, actions: [allow, deny], intentIdentifiers: [])
        let generic = UNNotificationCategory(identifier: categoryGeneric, actions: [], intentIdentifiers: [])
        UNUserNotificationCenter.current().setNotificationCategories([permission, generic])
    }

    static func post(_ n: AttentionNotification, requestId: String?, sound: Bool) {
        let content = UNMutableNotificationContent()
        content.title = n.title
        content.body = n.body
        content.categoryIdentifier = n.category
        content.userInfo = [sessionKey: n.sessionId, requestKey: requestId ?? ""]
        if sound { content.sound = .default }
        // One live notification per session: a newer attention replaces the old.
        let req = UNNotificationRequest(identifier: "session:\(n.sessionId)", content: content, trigger: nil)
        UNUserNotificationCenter.current().add(req)
    }

    static func clear(sessionId: String) {
        UNUserNotificationCenter.current().removeDeliveredNotifications(withIdentifiers: ["session:\(sessionId)"])
    }
}

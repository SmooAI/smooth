import AppKit
import CoreImage
import Foundation

/// Phone pairing for end-to-end encrypted relay frames (pearl th-d98fde).
/// The engine owns the keys; this app only shows the QR and the list.
/// Wire shapes are `/api/flow/pair*` in `crates/smooth-daemon/src/flow_pair_route.rs`.
struct PairingBegin: Decodable, Equatable {
    var pairingId: String
    var url: String
    var code: String
    var device: String
    var label: String
    var expiresAt: String?
    var relayEnabled: Bool

    enum CodingKeys: String, CodingKey {
        case pairingId = "pairing_id", url, code, device, label, expiresAt = "expires_at", relayEnabled = "relay_enabled"
    }
}

/// `GET /api/flow/pair/{id}`
struct PairingPoll: Decodable, Equatable {
    var state: String
    var device: String?
    var label: String?
    var platform: String?
    var expiresAt: String?

    enum CodingKeys: String, CodingKey { case state, device, label, platform, expiresAt = "expires_at" }
    var isPaired: Bool { state == "paired" }
    var isGone: Bool { state == "expired" || state == "unknown" }
}

/// One row of `GET /api/flow/pairings`.
struct PairedPhone: Decodable, Equatable, Identifiable {
    var device: String
    var label: String
    var platform: String
    var publicKey: String
    var createdAt: String
    var lastSeenAt: String?

    enum CodingKeys: String, CodingKey {
        case device, label, platform, publicKey = "public_key", createdAt = "created_at", lastSeenAt = "last_seen_at"
    }
    var id: String { device }
}

struct PairingsList: Decodable, Equatable {
    var device: String
    var label: String
    var relayEnabled: Bool
    /// The live relay link (th-37c286). Optional so an older engine that
    /// doesn't report it still decodes.
    var relay: RelayLinkStatus?
    var pairings: [PairedPhone]

    enum CodingKeys: String, CodingKey { case device, label, relayEnabled = "relay_enabled", relay, pairings }
}

/// What the engine's relay link is doing (`relay.state` from
/// `/api/flow/pairings`, th-37c286). The point is to keep three situations
/// apart that all look like "my phone can't see this Mac": signed out,
/// connected-but-not-authenticated, and genuinely unreachable.
struct RelayLinkStatus: Decodable, Equatable {
    var state: String
    var detail: String
    var since: String?

    /// Phones can reach this Mac only when the relay acknowledged it.
    var reachable: Bool { state == "online" }

    /// Needs the user (sign in, or a link that is up but not a peer) — amber.
    var needsAttention: Bool {
        ["signed_out", "session_expired", "unauthenticated", "auth_rejected", "identity_busy"].contains(state)
    }

    /// Short, human headline for the phase; `detail` carries the what-to-do.
    var headline: String {
        switch state {
        case "online": "On the relay — phones can reach this Mac"
        case "connecting": "Connecting to the relay…"
        case "authenticating": "Connected, waiting for the relay to authenticate"
        case "unauthenticated": "Connected but NOT authenticated — phones see this Mac as offline"
        case "auth_rejected": "The relay rejected this Mac's Smoo session"
        case "signed_out": "Not signed in to Smoo — not on the relay"
        case "session_expired": "Smoo session expired — not on the relay"
        case "identity_busy": "Another engine on this Mac holds the relay identity"
        case "offline": "Relay unreachable — retrying"
        case "disabled": "The relay is off for this engine"
        default: "Relay: \(state)"
        }
    }

    /// Form, not just color: ● reachable, ◐ on the way, ○ not on the relay.
    var glyph: String {
        switch state {
        case "online": "●"
        case "connecting", "authenticating": "◐"
        default: "○"
        }
    }
}

/// Presence of a paired phone, from its last-seen time — encoded in FORM (a
/// glyph) so it reads without color.
enum PhonePresence: Equatable {
    case here      // heard from in the last 5 minutes
    case recent    // within a day
    case away      // older, or never

    static func of(lastSeen iso: String?, now: Date = Date()) -> PhonePresence {
        guard let iso, let d = ISO8601DateFormatter.flexible.date(from: iso) else { return .away }
        let age = now.timeIntervalSince(d)
        if age < 5 * 60 { return .here }
        if age < 24 * 3600 { return .recent }
        return .away
    }

    var glyph: String {
        switch self {
        case .here: "●"
        case .recent: "◐"
        case .away: "○"
        }
    }

    var caption: String {
        switch self {
        case .here: "here"
        case .recent: "today"
        case .away: "away"
        }
    }
}

/// The pairing link as a QR (CoreImage, no dependency). Crisp at any size:
/// the generator's 1-pt modules are scaled up with nearest-neighbour.
enum PairingQR {
    static func image(for text: String, side: CGFloat = 220) -> NSImage? {
        guard let filter = CIFilter(name: "CIQRCodeGenerator") else { return nil }
        filter.setValue(Data(text.utf8), forKey: "inputMessage")
        filter.setValue("M", forKey: "inputCorrectionLevel")
        guard let output = filter.outputImage else { return nil }
        let scale = side / max(output.extent.width, 1)
        let scaled = output.transformed(by: CGAffineTransform(scaleX: scale, y: scale))
        let rep = NSCIImageRep(ciImage: scaled)
        let img = NSImage(size: rep.size)
        img.addRepresentation(rep)
        return img
    }
}

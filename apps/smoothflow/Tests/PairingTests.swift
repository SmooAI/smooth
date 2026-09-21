import XCTest
@testable import SmoothFlow

/// Settings ▸ Phones models (th-d98fde): the engine's `/api/flow/pair*` JSON
/// decodes, presence is a glyph derived from last-seen, and the QR renders.
final class PairingTests: XCTestCase {
    func testBeginAndPollDecode() throws {
        let begin = """
        {"pairing_id":"1a2b3c4d","url":"smoothflow://pair?v=1&p=1a2b3c4d&d=daemon-0123456789ab&k=x&c=y&l=smoo-hub","code":"EDJUdpi63P4BI0VniavN7w",
         "device":"daemon-0123456789ab","label":"smoo-hub","daemon_public_key":"x","expires_at":"2026-09-09T10:05:00.000Z","relay_enabled":true}
        """
        let b = try JSONDecoder().decode(PairingBegin.self, from: Data(begin.utf8))
        XCTAssertEqual(b.pairingId, "1a2b3c4d")
        XCTAssertTrue(b.url.hasPrefix("smoothflow://pair?v=1&p=1a2b3c4d"))
        XCTAssertTrue(b.relayEnabled)

        let pending = try JSONDecoder().decode(PairingPoll.self, from: Data(#"{"state":"pending","pairing_id":"1a2b3c4d","expires_at":"2026-09-09T10:05:00.000Z"}"#.utf8))
        XCTAssertFalse(pending.isPaired); XCTAssertFalse(pending.isGone)
        let paired = try JSONDecoder().decode(PairingPoll.self, from: Data(#"{"state":"paired","pairing_id":"1a2b3c4d","device":"phone-1","label":"Brent's iPhone","platform":"ios"}"#.utf8))
        XCTAssertTrue(paired.isPaired); XCTAssertEqual(paired.label, "Brent's iPhone")
        let gone = try JSONDecoder().decode(PairingPoll.self, from: Data(#"{"state":"expired","pairing_id":"1a2b3c4d"}"#.utf8))
        XCTAssertTrue(gone.isGone)
    }

    func testPairingsListDecodesWithoutTheKey() throws {
        let raw = """
        {"device":"daemon-0123456789ab","label":"smoo-hub","relay_enabled":false,
         "pairings":[{"device":"phone-1","label":"Pixel","platform":"android","public_key":"pk","created_at":"2026-09-01T12:00:00.000Z","last_seen_at":null}]}
        """
        let l = try JSONDecoder().decode(PairingsList.self, from: Data(raw.utf8))
        XCTAssertFalse(l.relayEnabled)
        XCTAssertEqual(l.pairings.count, 1)
        XCTAssertEqual(l.pairings[0].id, "phone-1")
        XCTAssertNil(l.pairings[0].lastSeenAt)
    }

    func testRelayLinkStatusDecodesAndKeepsTheCantReachCasesApart() throws {
        let raw = """
        {"device":"daemon-0123456789ab","label":"marvin","relay_enabled":true,
         "relay":{"state":"unauthenticated","detail":"no ack in 15s","since":"2026-09-20T12:00:00Z"},"pairings":[]}
        """
        let l = try JSONDecoder().decode(PairingsList.self, from: Data(raw.utf8))
        let r = try XCTUnwrap(l.relay)
        XCTAssertEqual(r.state, "unauthenticated")
        XCTAssertFalse(r.reachable, "connected-but-unauthenticated is not reachable")
        XCTAssertTrue(r.needsAttention)
        XCTAssertTrue(r.headline.contains("NOT authenticated"))

        let online = RelayLinkStatus(state: "online", detail: "", since: nil)
        XCTAssertTrue(online.reachable); XCTAssertFalse(online.needsAttention); XCTAssertEqual(online.glyph, "●")
        let signedOut = RelayLinkStatus(state: "signed_out", detail: "", since: nil)
        XCTAssertFalse(signedOut.reachable); XCTAssertTrue(signedOut.needsAttention)
        XCTAssertNotEqual(signedOut.headline, r.headline, "signed out and unauthenticated must read differently")
        let authenticating = RelayLinkStatus(state: "authenticating", detail: "", since: nil)
        XCTAssertFalse(authenticating.reachable); XCTAssertEqual(authenticating.glyph, "◐")
    }

    func testAnOlderEngineWithoutRelayStatusStillDecodes() throws {
        let raw = #"{"device":"d","label":"l","relay_enabled":true,"pairings":[]}"#
        XCTAssertNil(try JSONDecoder().decode(PairingsList.self, from: Data(raw.utf8)).relay)
    }

    func testPresenceIsAGlyphFromLastSeen() {
        let now = ISO8601DateFormatter().date(from: "2026-09-09T12:00:00Z")!
        XCTAssertEqual(PhonePresence.of(lastSeen: nil, now: now), .away)
        XCTAssertEqual(PhonePresence.of(lastSeen: "garbage", now: now), .away)
        XCTAssertEqual(PhonePresence.of(lastSeen: "2026-09-09T11:58:00.000Z", now: now), .here)
        XCTAssertEqual(PhonePresence.of(lastSeen: "2026-09-09T02:00:00.000Z", now: now), .recent)
        XCTAssertEqual(PhonePresence.of(lastSeen: "2026-09-01T02:00:00.000Z", now: now), .away)
        XCTAssertEqual(Set([PhonePresence.here.glyph, PhonePresence.recent.glyph, PhonePresence.away.glyph]).count, 3, "three distinct glyphs — state is carried in form, not color")
    }

    func testQRRendersASquareImage() {
        let img = PairingQR.image(for: "smoothflow://pair?v=1&p=1a2b3c4d&d=daemon-0123456789ab&k=hSDwCYkwp1R0i33ctD73Wg2_Og0mOBr06NxOql-OKqo&c=EDJUdpi63P4BI0VniavN7w&l=smoo-hub", side: 200)
        XCTAssertNotNil(img)
        XCTAssertEqual(img?.size.width, img?.size.height)
        XCTAssertGreaterThanOrEqual(img?.size.width ?? 0, 199)
    }
}

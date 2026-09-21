import XCTest
@testable import SmoothFlow

/// th-a1bb12: the child daemon must never share Big Smooth's relay device id.
final class RelayIdentityTests: XCTestCase {
    func testPersistedIdIsReusedVerbatim() {
        let r = RelayIdentity.deviceId(persisted: "daemon-abc123def456\n", mint: { XCTFail("must not mint"); return "x" })
        XCTAssertEqual(r.id, "daemon-abc123def456")
        XCTAssertFalse(r.fresh)
    }

    func testMissingOrMalformedIdIsMinted() {
        for bad in [nil, "", "   ", "has space", "has:colon", String(repeating: "a", count: 65), "emoji-🎉"] {
            let r = RelayIdentity.deviceId(persisted: bad, mint: { "daemon-minted00000" })
            XCTAssertEqual(r.id, "daemon-minted00000", "persisted \(bad ?? "nil")")
            XCTAssertTrue(r.fresh)
        }
    }

    func testMintedIdMatchesTheDaemonsShapeAndTheRelayGrammar() {
        let id = RelayIdentity.mintDeviceId()
        XCTAssertTrue(id.hasPrefix("daemon-"), id)
        XCTAssertEqual(id.count, "daemon-".count + 12, id)
        XCTAssertTrue(RelayIdentity.isValidDeviceId(id), id)
        XCTAssertNotEqual(id, RelayIdentity.mintDeviceId(), "unique per mint")
    }

    func testLabelIsTheShortHostnamePlusSmoothFlow() {
        XCTAssertEqual(RelayIdentity.label(hostname: "smoo-hub.local"), "smoo-hub · SmoothFlow")
        XCTAssertEqual(RelayIdentity.label(hostname: "  Brent’s MacBook Pro\n"), "Brent’s MacBook Pro · SmoothFlow")
        XCTAssertEqual(RelayIdentity.label(hostname: nil), "big-smooth · SmoothFlow")
        XCTAssertEqual(RelayIdentity.label(hostname: ""), "big-smooth · SmoothFlow")
    }

    func testEnvironmentCarriesIdLabelAndFlowKind() {
        let env = RelayIdentity.environment(deviceId: "daemon-abc123def456", hostname: "smoo-hub")
        XCTAssertEqual(env["SMOOTH_RELAY_DEVICE_ID"], "daemon-abc123def456")
        XCTAssertEqual(env["SMOOTH_RELAY_LABEL"], "smoo-hub · SmoothFlow")
        XCTAssertEqual(env["SMOOTH_RELAY_KIND"], "flow")
        XCTAssertEqual(env.count, 3, "exactly the daemon's three relay knobs")
    }

    func testLoadMintsOncePersistsAndReloads() throws {
        let home = FileManager.default.temporaryDirectory.appendingPathComponent("relay-identity-\(UUID().uuidString.prefix(8))")
        defer { try? FileManager.default.removeItem(at: home) }
        let first = RelayIdentity.load(home: home)
        XCTAssertTrue(RelayIdentity.isValidDeviceId(first))
        let file = home.appendingPathComponent(RelayIdentity.deviceIdFile)
        XCTAssertEqual(try String(contentsOf: file, encoding: .utf8), first + "\n")
        let perms = try FileManager.default.attributesOfItem(atPath: file.path)[.posixPermissions] as? Int
        XCTAssertEqual(perms, 0o600, "owner-only like the daemon's relay-device-id")
        XCTAssertEqual(RelayIdentity.load(home: home), first, "stable across launches")
        // Never Big Smooth's file.
        XCTAssertFalse(FileManager.default.fileExists(atPath: home.appendingPathComponent(".smooth/relay-device-id").path))
    }

    func testLoadReplacesAJunkFile() throws {
        let home = FileManager.default.temporaryDirectory.appendingPathComponent("relay-identity-\(UUID().uuidString.prefix(8))")
        defer { try? FileManager.default.removeItem(at: home) }
        let file = home.appendingPathComponent(RelayIdentity.deviceIdFile)
        try FileManager.default.createDirectory(at: file.deletingLastPathComponent(), withIntermediateDirectories: true)
        try "not a device id!".write(to: file, atomically: true, encoding: .utf8)
        let id = RelayIdentity.load(home: home)
        XCTAssertTrue(RelayIdentity.isValidDeviceId(id))
        XCTAssertEqual(try String(contentsOf: file, encoding: .utf8), id + "\n", "rewritten with the minted id")
    }
}

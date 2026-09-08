import XCTest
@testable import SmoothFlow

final class DaemonAddressTests: XCTestCase {
    func testParseShapes() {
        XCTAssertEqual(DaemonAddress.parse("127.0.0.1:8790"), .init(host: "127.0.0.1", port: 8790))
        XCTAssertEqual(DaemonAddress.parse("8790"), .init(host: "127.0.0.1", port: 8790))
        XCTAssertEqual(DaemonAddress.parse(" ws://localhost:4400/api/flow/ws \n"), .init(host: "localhost", port: 4400))
        XCTAssertEqual(DaemonAddress.parse("http://smoo-hub:8788"), .init(host: "smoo-hub", port: 8788))
        XCTAssertEqual(DaemonAddress.parse(":8791"), .init(host: "127.0.0.1", port: 8791))
        XCTAssertNil(DaemonAddress.parse(""))
        XCTAssertNil(DaemonAddress.parse("nonsense"))
        XCTAssertNil(DaemonAddress.parse("host:0"))
        XCTAssertNil(DaemonAddress.parse("host:99999"))
    }

    func testEndpointURLs() {
        let e = DaemonAddress.Endpoint(host: "127.0.0.1", port: 8790)
        XCTAssertEqual(e.wsURL.absoluteString, "ws://127.0.0.1:8790/api/flow/ws")
        XCTAssertEqual(e.httpBase.appendingPathComponent("api/flow/sessions/x/handoff").absoluteString, "http://127.0.0.1:8790/api/flow/sessions/x/handoff")
    }

    func testResolutionOrderEnvThenSettingThenSpawn() {
        XCTAssertEqual(DaemonAddress.resolve(env: [DaemonAddress.envKey: "127.0.0.1:1"], setting: "127.0.0.1:2"), .external(.init(host: "127.0.0.1", port: 1)))
        XCTAssertEqual(DaemonAddress.resolve(env: [:], setting: "127.0.0.1:2"), .external(.init(host: "127.0.0.1", port: 2)))
        XCTAssertEqual(DaemonAddress.resolve(env: [DaemonAddress.envKey: "garbage"], setting: nil), .spawn)
        XCTAssertEqual(DaemonAddress.resolve(env: [:], setting: ""), .spawn, "default is to own the daemon, never adopt a terminal's")
    }

    func testDaemonBinaryPreference() {
        let home = URL(fileURLWithPath: "/Users/u")
        let bundle = URL(fileURLWithPath: "/Applications/SmoothFlow.app/Contents/MacOS")
        let bundled = "/Applications/SmoothFlow.app/Contents/MacOS/smooth-daemon"
        let cargo = "/Users/u/.cargo/bin/smooth-daemon"
        let brew = "/opt/homebrew/bin/smooth-daemon"
        XCTAssertEqual(DaemonAddress.daemonBinary(bundleExecutableDir: bundle, home: home, path: "/opt/homebrew/bin", exists: { [bundled, cargo, brew].contains($0) }), bundled)
        XCTAssertEqual(DaemonAddress.daemonBinary(bundleExecutableDir: bundle, home: home, path: "/opt/homebrew/bin", exists: { [cargo, brew].contains($0) }), cargo)
        XCTAssertEqual(DaemonAddress.daemonBinary(bundleExecutableDir: nil, home: home, path: "/usr/bin:/opt/homebrew/bin", exists: { $0 == brew }), brew)
        XCTAssertNil(DaemonAddress.daemonBinary(bundleExecutableDir: nil, home: home, path: "", exists: { _ in false }))
    }

    func testAddrFile() {
        XCTAssertEqual(DaemonAddress.readAddrFile("127.0.0.1:8899\n"), .init(host: "127.0.0.1", port: 8899))
    }
}

final class DaemonAddressIntegrationTests: XCTestCase {
    func testBinaryOverrideWinsAndIsSkippedWhenMissing() {
        let home = URL(fileURLWithPath: "/Users/u")
        let bundle = URL(fileURLWithPath: "/Applications/SmoothFlow.app/Contents/MacOS")
        let bundled = "/Applications/SmoothFlow.app/Contents/MacOS/smooth-daemon"
        let dev = "/Users/u/.cargo/target-x/release/smooth-daemon"
        XCTAssertEqual(DaemonAddress.daemonBinary(override: dev, bundleExecutableDir: bundle, home: home, path: "", exists: { [bundled, dev].contains($0) }), dev)
        XCTAssertEqual(DaemonAddress.daemonBinary(override: "~/.cargo/target-x/release/smooth-daemon", bundleExecutableDir: bundle, home: home, path: "",
                                                  exists: { $0.hasPrefix(NSHomeDirectory()) || $0 == bundled }),
                       (("~/.cargo/target-x/release/smooth-daemon") as NSString).expandingTildeInPath, "tilde expands")
        XCTAssertEqual(DaemonAddress.daemonBinary(override: "/nope", bundleExecutableDir: bundle, home: home, path: "", exists: { $0 == bundled }), bundled, "a missing override falls through")
        XCTAssertEqual(DaemonAddress.daemonBinary(override: "  ", bundleExecutableDir: bundle, home: home, path: "", exists: { $0 == bundled }), bundled)
    }

    func testTokenResolutionOrder() {
        XCTAssertEqual(DaemonAddress.token(env: [DaemonAddress.tokenEnvKey: "a", "SMOOTH_LOCAL_TOKEN": "b"], tokenFile: "c\n"), "a")
        XCTAssertEqual(DaemonAddress.token(env: ["SMOOTH_LOCAL_TOKEN": " b "], tokenFile: "c\n"), "b")
        XCTAssertEqual(DaemonAddress.token(env: [:], tokenFile: "c\n"), "c")
        XCTAssertNil(DaemonAddress.token(env: [DaemonAddress.tokenEnvKey: "  "], tokenFile: "  \n"))
        XCTAssertNil(DaemonAddress.token(env: [:], tokenFile: nil))
    }

    func testEndpointCarriesTokenOnTheWebSocketOnly() {
        let e = DaemonAddress.Endpoint(host: "127.0.0.1", port: 8790, token: "t k")
        XCTAssertEqual(e.wsURL.absoluteString, "ws://127.0.0.1:8790/api/flow/ws?token=t%20k")
        XCTAssertEqual(e.httpBase.absoluteString, "http://127.0.0.1:8790/", "HTTP presents it as X-Smooth-Token, not in the URL")
        XCTAssertEqual(DaemonAddress.Endpoint(host: "h", port: 1).wsURL.absoluteString, "ws://h:1/api/flow/ws")
    }
}

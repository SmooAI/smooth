import AppKit
import XCTest

/// th-bcd819: Nerd Font / Unicode glyphs survive the whole path — a shell in
/// the real engine's tmux (started without any locale, like a Finder-launched
/// app's child), the engine's attach client, `flow.output`, the pane. In 0.2.1
/// a starship prompt came out as `_` and blanks: tmux treated the LANG-less
/// attach client as non-UTF-8.
final class GlyphUITests: FlowUITestCase {
    private var id = ""

    override func setUpWithError() throws {
        try super.setUpWithError()
        try startEngine()
        try launchApp()
        id = try newSession(["kind": "shell", "argv": ["/bin/sh"]])
        XCTAssertTrue(app.staticTexts["sidebar.title.\(id)"].waitForExistence(timeout: 15), "shell session in the sidebar")
        app.staticTexts["sidebar.title.\(id)"].click()
    }

    func testShellGlyphsReachThePaneIntact() throws {
        let before = Self.brightPixels(app.windows["SmoothFlow"].screenshot().image)
        // What a starship prompt prints: the prompt char, a Powerline branch
        // icon (Nerd Font private use), a cloud.
        let r = http("POST", "/api/flow/sessions/\(id)/send", body: ["text": "printf 'GLYPHS \\342\\235\\257 \\356\\202\\240 \\342\\230\\201 END\\n'"])
        XCTAssertEqual(r.status, 200, r.text)
        // 1. The engine's own view of the pane (server side) has them…
        waitForSnapshot(id, containing: "GLYPHS ❯ \u{e0a0} ☁ END")
        // 2. …and so do the bytes the app was streamed through the attach client.
        // Wait for the RENDERED line, not the shell's echo of the printf (which
        // also contains "END", with backslashes): cloud-or-underscore then END.
        let streamed = try streamedOutput(until: ["☁ END", "_ END"])
        XCTAssertTrue(streamed.contains("GLYPHS ❯ \u{e0a0} ☁ END"), "flow.output carried the UTF-8 intact, got: \(streamed.suffix(200))")
        XCTAssertFalse(streamed.contains("GLYPHS _ _ _"), "tmux drew underscores — a non-UTF-8 attach client")
        // 3. …and the pane drew something for them (not blank cells).
        let after = Self.brightPixels(app.windows["SmoothFlow"].screenshot().image)
        XCTAssertGreaterThan(after, before + 40, "the glyph line lit up pixels in the pane")
    }

    /// Attach to the session over the flow WebSocket as a second client and
    /// collect `flow.output` until `marker` shows up (or 20 s pass).
    private func streamedOutput(until markers: [String]) throws -> String {
        var comps = URLComponents(string: "ws://\(addr)/api/flow/ws")!
        if let token { comps.queryItems = [URLQueryItem(name: "token", value: token)] }
        let task = URLSession.shared.webSocketTask(with: comps.url!)
        task.resume()
        let attach = try JSONSerialization.data(withJSONObject: ["channel": "flow", "type": "flow.attach", "id": id, "cols": 100, "rows": 30])
        task.send(.string(String(decoding: attach, as: UTF8.self))) { _ in }
        var collected = ""
        let deadline = Date().addingTimeInterval(20)
        while Date() < deadline, !markers.contains(where: { collected.contains($0) }) {
            let sem = DispatchSemaphore(value: 0)
            task.receive { result in
                if case let .success(.string(text)) = result,
                   let obj = try? JSONSerialization.jsonObject(with: Data(text.utf8)) as? [String: Any],
                   obj["type"] as? String == "flow.output", obj["id"] as? String == self.id,
                   let b64 = obj["data_b64"] as? String, let data = Data(base64Encoded: b64) {
                    collected += String(decoding: data, as: UTF8.self)
                }
                sem.signal()
            }
            _ = sem.wait(timeout: .now() + 3)
        }
        task.cancel(with: .normalClosure, reason: nil)
        // Strip the escapes the shell and tmux add so the assertion reads plain text.
        return collected.replacingOccurrences(of: "\u{1b}\\[[0-9;?]*[A-Za-z]", with: "", options: .regularExpression)
    }

    /// Pixels clearly lighter than the dark pane ground.
    static func brightPixels(_ image: NSImage) -> Int {
        guard let tiff = image.tiffRepresentation, let rep = NSBitmapImageRep(data: tiff) else { return 0 }
        var n = 0
        for y in stride(from: 0, to: rep.pixelsHigh, by: 2) {
            for x in stride(from: 0, to: rep.pixelsWide, by: 2) {
                if let c = rep.colorAt(x: x, y: y), c.brightnessComponent > 0.6 { n += 1 }
            }
        }
        return n
    }
}

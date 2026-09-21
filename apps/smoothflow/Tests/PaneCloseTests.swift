@testable import SmoothFlow
import XCTest

/// The ⌘W decision — does this pane need confirming, and what does the prompt
/// say — as a pure function over pane state. The `NSAlert` that presents it is
/// not tested here; the rules are the part with something to get wrong.
final class PaneCloseTests: XCTestCase {
    private func session(_ kind: String, _ state: SessionState, pearl: String? = "th-27baa4", title: String = "a title") -> Session {
        Session(id: "fs-1", kind: kind, title: title, pearlId: pearl, state: state)
    }

    // MARK: when nothing is asked

    func testEmptyPaneClosesSilently() {
        let d = PaneClose.decide(session: nil, harnessLabel: "", scope: .pane)
        XCTAssertNil(d.prompt)
        XCTAssertEqual(d.scope, .pane)
    }

    func testFinishedOrDeadSessionClosesSilently() {
        for state in [SessionState.done, .dead] {
            XCTAssertNil(PaneClose.decide(session: session("claude", state), harnessLabel: "Claude Code", scope: .pane).prompt,
                         "a \(state.rawValue) session is scrollback, not a process")
        }
    }

    /// Ghostty's `confirm-close-surface` nuance: a shell at a prompt has
    /// nothing to lose, and an alert people learn to dismiss unread is worse
    /// than no alert.
    func testIdleShellClosesSilently() {
        XCTAssertNil(PaneClose.decide(session: session("shell", .idle, pearl: nil), harnessLabel: "Shell", scope: .pane).prompt)
        XCTAssertFalse(PaneClose.hasRunningProcess(session("shell", .idle, pearl: nil)))
    }

    func testShellWithAForegroundProcessAsks() throws {
        let d = PaneClose.decide(session: session("shell", .working, pearl: nil, title: "shell · smooth"), harnessLabel: "Shell", scope: .pane)
        let prompt = try XCTUnwrap(d.prompt)
        XCTAssertTrue(prompt.message.contains("Something is still running"), prompt.message)
        XCTAssertTrue(prompt.message.contains("shell · smooth"), prompt.message)
        XCTAssertEqual(prompt.killTitle, "End Session")
        XCTAssertTrue(PaneClose.hasRunningProcess(session("shell", .working, pearl: nil)))
    }

    /// ⌘T starts the new tab on the focused session, so ⌘T-then-⌘W would hit a
    /// dialog about a session that is still sitting in the tab you came from.
    /// Closing one of several views of a session destroys nothing.
    func testSessionStillVisibleElsewhereClosesSilently() {
        let live = session("claude", .working)
        XCTAssertNotNil(PaneClose.decide(session: live, harnessLabel: "Claude Code", scope: .pane).prompt,
                        "the only view of it still asks")
        XCTAssertNil(PaneClose.decide(session: live, harnessLabel: "Claude Code", scope: .pane, shownElsewhere: true).prompt,
                     "a duplicate view does not")
    }

    func testSuppressedConfirmationNeverAsks() {
        let d = PaneClose.decide(session: session("claude", .working), harnessLabel: "Claude Code", scope: .pane, confirmEnabled: false)
        XCTAssertNil(d.prompt, "with the setting off, ⌘W closes the view and never kills anything")
    }

    // MARK: what the prompt says

    /// The dangerous case, and the whole reason for the dialog: it must name
    /// the harness, name the work, and say plainly that ending the session
    /// kills the process.
    func testAgentPromptNamesTheHarnessTheWorkAndTheCost() throws {
        let d = PaneClose.decide(session: session("claude", .working), harnessLabel: "Claude Code", scope: .pane)
        let prompt = try XCTUnwrap(d.prompt)
        XCTAssertEqual(prompt.title, "Close this pane?")
        XCTAssertTrue(prompt.message.contains("Claude Code"), prompt.message)
        XCTAssertTrue(prompt.message.contains("th-27baa4"), prompt.message)
        XCTAssertTrue(prompt.message.contains("still working"), prompt.message)
        XCTAssertTrue(prompt.message.contains("kills the process"), prompt.message)
        XCTAssertTrue(prompt.message.contains("loses the work in flight"), prompt.message)
        XCTAssertTrue(prompt.message.contains("leaves it running"), "the safe answer has to be stated too: \(prompt.message)")
        XCTAssertEqual(prompt.closeTitle, "Close Pane")
        XCTAssertEqual(prompt.killTitle, "End Session")
    }

    func testPromptDescribesTheStateItFound() throws {
        let cases: [(SessionState, String)] = [
            (.working, "still working"), (.starting, "still working"),
            (.needsYou, "waiting on you"), (.limited, "paused on a usage limit"), (.unknown, "open"),
        ]
        for (state, phrase) in cases {
            let prompt = try XCTUnwrap(PaneClose.decide(session: session("claude", state), harnessLabel: "Claude Code", scope: .pane).prompt)
            XCTAssertTrue(prompt.message.contains(phrase), "\(state.rawValue): \(prompt.message)")
        }
    }

    /// ⌘W takes the container with it when the pane was the last one, and the
    /// prompt has to say which — "Close Pane" on the sheet that is about to
    /// close your window would be a lie.
    func testScopeChangesTheTitleAndTheButton() throws {
        let expected: [(PaneCloseScope, String, String)] = [
            (.pane, "Close this pane?", "Close Pane"),
            (.tab, "Close this tab?", "Close Tab"),
            (.window, "Close this window?", "Close Window"),
        ]
        for (scope, title, button) in expected {
            let prompt = try XCTUnwrap(PaneClose.decide(session: session("claude", .working), harnessLabel: "Claude Code", scope: scope).prompt)
            XCTAssertEqual(prompt.title, title)
            XCTAssertEqual(prompt.closeTitle, button)
            XCTAssertTrue(prompt.message.contains(scope.noun.lowercased()), prompt.message)
        }
    }

    func testFallsBackToTheTitleThenTheIdWhenThereIsNoPearl() throws {
        let titled = try XCTUnwrap(PaneClose.decide(session: session("codex", .working, pearl: nil, title: "refactor the parser"),
                                                   harnessLabel: "Codex", scope: .pane).prompt)
        XCTAssertTrue(titled.message.contains("refactor the parser"), titled.message)
        let bare = try XCTUnwrap(PaneClose.decide(session: session("codex", .working, pearl: nil, title: ""),
                                                 harnessLabel: "Codex", scope: .pane).prompt)
        XCTAssertTrue(bare.message.contains("fs-1"), bare.message)
    }

    func testFallsBackToTheRawKindWhenTheHarnessIsUnknown() throws {
        let prompt = try XCTUnwrap(PaneClose.decide(session: session("opencode", .working), harnessLabel: "", scope: .pane).prompt)
        XCTAssertTrue(prompt.message.hasPrefix("opencode is"), prompt.message)
    }

    // MARK: the setting

    func testConfirmDefaultsOnAndRoundTrips() {
        let d = UserDefaults(suiteName: "pane-close-\(UUID().uuidString)")!
        XCTAssertTrue(PaneCloseSettings.confirm(d), "asking is the default")
        PaneCloseSettings.setConfirm(false, d)
        XCTAssertFalse(PaneCloseSettings.confirm(d))
        PaneCloseSettings.setConfirm(true, d)
        XCTAssertTrue(PaneCloseSettings.confirm(d))
    }
}

/// ⌘W / ⌘⇧W as bindings — the half of the spec that lives in the keymap.
final class PaneCloseKeymapTests: XCTestCase {
    func testCommandWIsClosePaneAndCommandShiftWIsCloseTab() {
        XCTAssertEqual(Keymap.default.chord(for: .closePane), KeyChord("w", command: true))
        XCTAssertEqual(Keymap.default.chord(for: .closeTab), KeyChord("w", command: true, shift: true))
        XCTAssertTrue(Keymap.default.conflicts.isEmpty)
    }

    func testClosePaneIsInTheTabsCategoryWithARowOfItsOwn() {
        XCTAssertEqual(FlowAction.closePane.category, .tabs)
        XCTAssertEqual(FlowAction.closePane.title, "Close Pane")
        XCTAssertNotNil(FlowAction.closePane.note, "the row explains the collapse-when-empty rule")
    }

    func testClosePaneIsRebindable() {
        let map = Keymap.parse("""
        [keys]
        closePane = "ctrl+shift+w"
        """)
        XCTAssertEqual(map.chord(for: .closePane), KeyChord("w", shift: true, control: true))
        XCTAssertEqual(map.chord(for: .closeTab), FlowAction.closeTab.defaultChord)
    }
}

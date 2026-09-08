import XCTest
@testable import SmoothFlow

final class AttentionNotifierTests: XCTestCase {
    private func session(_ reason: AttentionReason?, state: SessionState = .needsYou, detail: String? = nil, resumeAt: String? = nil, pid: Int? = nil) -> Session {
        Session(id: "fs-1", title: "labels column", pearlId: "th-1", state: state,
                attention: reason.map { Attention(reason: $0, detail: detail, resumeAt: resumeAt, requestId: "r", pid: pid) })
    }

    func testPermissionMapsToActionableCategory() {
        let n = AttentionNotifier.notification(for: session(.permission, detail: "git push"), settings: NotifySettings())
        XCTAssertEqual(n?.title, "th-1 needs approval")
        XCTAssertEqual(n?.body, "git push")
        XCTAssertEqual(n?.category, AttentionNotifier.categoryPermission)
        XCTAssertEqual(n?.sessionId, "fs-1")
    }

    func testEveryReasonHasASettingThatSilencesIt() {
        let cases: [(AttentionReason, WritableKeyPath<NotifySettings, Bool>)] = [
            (.permission, \.permission), (.question, \.question), (.usageLimit, \.usageLimit), (.crashed, \.crashed), (.held, \.held),
        ]
        for (reason, key) in cases {
            XCTAssertNotNil(AttentionNotifier.notification(for: session(reason), settings: NotifySettings()), "\(reason) on")
            var off = NotifySettings()
            off[keyPath: key] = false
            XCTAssertNil(AttentionNotifier.notification(for: session(reason), settings: off), "\(reason) off")
        }
    }

    func testUsageLimitMentionsResumeTime() {
        let n = AttentionNotifier.notification(for: session(.usageLimit, state: .limited, resumeAt: "2026-09-07T16:00:00Z"), settings: NotifySettings())
        XCTAssertEqual(n?.title, "th-1 hit the usage limit")
        XCTAssertTrue(n?.body.contains("resumes") == true, n?.body ?? "")
        XCTAssertEqual(n?.category, AttentionNotifier.categoryGeneric)
    }

    func testHeldFallsBackToPid() {
        let n = AttentionNotifier.notification(for: session(.held, state: .idle, pid: 48122), settings: NotifySettings())
        XCTAssertEqual(n?.body, "pid 48122")
    }

    func testFinishedAndDead() {
        let done = AttentionNotifier.notification(for: session(nil, state: .done), settings: NotifySettings())
        XCTAssertEqual(done?.title, "th-1 finished")
        var dead = session(nil, state: .dead)
        dead.exitCode = 137
        XCTAssertEqual(AttentionNotifier.notification(for: dead, settings: NotifySettings())?.title, "th-1 died (exit 137)")
        var off = NotifySettings()
        off.finished = false
        XCTAssertNil(AttentionNotifier.notification(for: dead, settings: off))
    }

    func testNothingToSayIsNil() {
        XCTAssertNil(AttentionNotifier.notification(for: session(nil, state: .working), settings: NotifySettings()))
        XCTAssertNil(AttentionNotifier.notification(for: session(.unknown), settings: NotifySettings()))
    }

    func testSettingsRoundTripThroughDefaults() {
        let d = UserDefaults(suiteName: "smoothflow.tests.\(UUID())")!
        var s = NotifySettings()
        s.question = false
        s.sound = false
        s.save(d)
        let back = NotifySettings.load(d)
        XCTAssertEqual(back, s)
        XCTAssertTrue(NotifySettings.load(UserDefaults(suiteName: "smoothflow.tests.empty.\(UUID())")!).permission, "defaults are on")
    }
}

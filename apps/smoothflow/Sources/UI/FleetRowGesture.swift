import AppKit
import SwiftUI

/// Mouse gestures on a fleet sidebar row (th-f958f2), apart from AppKit so the
/// mapping can be unit-tested.
///
/// Middle-click (button 2) closes the row's session out — the same Close Out
/// as ⌘⌥W and the row's "Close Out…" menu item, sheet and all. It never closes
/// anything by itself: it opens the confirmation, and an engine refusal comes
/// back in its own sheet.
enum FleetRowGesture {
    /// `NSEvent.buttonNumber` of the middle button (0 left, 1 right).
    static let middleButton = 2

    /// The action a click with `button` on a fleet row runs, if any. Left and
    /// right clicks stay with the list (select / context menu).
    static func action(forButton button: Int) -> FlowAction? {
        button == middleButton ? .closeOut : nil
    }

    /// The action for a click with `button` at `point` (in the row's own
    /// coordinates), or nil when it lands outside the row's `bounds` or in
    /// another window.
    static func action(forButton button: Int, at point: CGPoint, in bounds: CGRect, sameWindow: Bool) -> FlowAction? {
        guard sameWindow, bounds.contains(point) else { return nil }
        return action(forButton: button)
    }
}

/// Catches a middle-click over the view it backs and hands the mapped
/// `FlowAction` to `perform`. SwiftUI has no middle-click gesture, and a view
/// on top would swallow the list's own clicks, so this watches the window's
/// `otherMouseDown` events through a local monitor instead and claims only the
/// ones that land on its row. Left and right clicks never reach it.
struct FleetRowMouse: NSViewRepresentable {
    var perform: (FlowAction) -> Void

    func makeNSView(context: Context) -> CatcherView {
        let v = CatcherView()
        v.perform = perform
        return v
    }

    func updateNSView(_ v: CatcherView, context: Context) { v.perform = perform }

    static func dismantleNSView(_ v: CatcherView, coordinator: ()) { v.stop() }

    final class CatcherView: NSView {
        var perform: ((FlowAction) -> Void)?
        private var monitor: Any?

        /// Never the target of a click: the list underneath keeps every
        /// left/right click, selection and context menu.
        override func hitTest(_ point: NSPoint) -> NSView? { nil }

        override func viewDidMoveToWindow() {
            super.viewDidMoveToWindow()
            if window == nil { stop() } else { start() }
        }

        private func start() {
            guard monitor == nil else { return }
            monitor = NSEvent.addLocalMonitorForEvents(matching: .otherMouseDown) { [weak self] event in
                guard let self, let window = self.window, !self.isHiddenOrHasHiddenAncestor,
                      let action = FleetRowGesture.action(forButton: event.buttonNumber,
                                                          at: self.convert(event.locationInWindow, from: nil),
                                                          in: self.bounds, sameWindow: event.window === window)
                else { return event }
                self.perform?(action)
                return nil
            }
        }

        func stop() {
            if let m = monitor { NSEvent.removeMonitor(m) }
            monitor = nil
        }
    }
}

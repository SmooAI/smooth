import AppKit
import SwiftUI

/// Sidebar · center · pearl rail.
@MainActor
final class MainWindowController: NSWindowController {
    let center: CenterViewController
    private let splitVC = NSSplitViewController()

    init(app: AppController) {
        center = CenterViewController(app: app)
        let w = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 1320, height: 820),
                         styleMask: [.titled, .closable, .miniaturizable, .resizable],
                         backing: .buffered, defer: false)
        w.title = "SmoothFlow"
        w.titlebarAppearsTransparent = false
        w.setFrameAutosaveName("SmoothFlow.main")
        w.minSize = NSSize(width: 900, height: 500)
        super.init(window: w)

        let sidebar = NSSplitViewItem(sidebarWithViewController: NSHostingController(rootView: FleetSidebar(store: app.store, app: app)))
        sidebar.minimumThickness = 220
        sidebar.canCollapse = true
        let main = NSSplitViewItem(viewController: center)
        main.minimumThickness = 420
        let rail = NSSplitViewItem(viewController: NSHostingController(rootView: PearlRail(store: app.store, app: app)))
        rail.minimumThickness = 240
        rail.canCollapse = true
        splitVC.addSplitViewItem(sidebar)
        splitVC.addSplitViewItem(main)
        splitVC.addSplitViewItem(rail)
        w.contentViewController = splitVC
        if !w.setFrameUsingName("SmoothFlow.main") { w.setFrame(NSRect(x: 60, y: 120, width: 1320, height: 820), display: true) }
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { nil }

    func toggleRail() { splitVC.splitViewItems.last?.animator().isCollapsed.toggle() }
    func toggleSidebar() { splitVC.splitViewItems.first?.animator().isCollapsed.toggle() }
}

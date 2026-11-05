import AppKit
import os
import WebKit

/// How every browser pane's web view is set up. The whole trust boundary is
/// here and in `BrowserURL`, so a test can read it.
///
/// - **No bridge.** No script message handlers and no user scripts: a page gets
///   nothing from the app that it would not get from Safari.
/// - **Non-persistent site data, one store shared by the panes.** Cookies and
///   storage live in memory and end with the app, so nothing a page leaves is on
///   disk, and a link an agent printed never lands in a long-lived signed-in
///   session. The cost is signing in again after a relaunch. Panes share the
///   store so a sign-in in one reaches the others during the run.
@MainActor
enum BrowserWebSetup {
    static let dataStore = WKWebsiteDataStore.nonPersistent()

    static func configuration() -> WKWebViewConfiguration {
        let config = WKWebViewConfiguration()
        config.websiteDataStore = dataStore
        config.preferences.javaScriptCanOpenWindowsAutomatically = false
        return config
    }
}

/// When a web view may take the keyboard. A page calling `focus()` reaches
/// AppKit as an ordinary request for first responder, so the view asks whether a
/// person just did something that means it: clicked in it or tabbed to it. The
/// event must be recent because `NSApp.currentEvent` stays set long after it was
/// handled, and a page can ask at any time.
enum BrowserKeyboard {
    private static let tabKey: UInt16 = 48
    private static let recent: TimeInterval = 1

    static func userAsked(by event: NSEvent?, for view: NSView, now: TimeInterval = ProcessInfo.processInfo.systemUptime) -> Bool {
        guard let event, event.window === view.window, view.window != nil, now - event.timestamp < recent else { return false }
        switch event.type {
        case .leftMouseDown, .rightMouseDown, .otherMouseDown:
            return view.bounds.contains(view.convert(event.locationInWindow, from: nil))
        case .keyDown:
            return event.keyCode == tabKey
        default:
            return false
        }
    }
}

/// A web view that says when the keyboard arrives, so its pane becomes the
/// focused one, the way a terminal surface does, and that a page cannot give
/// the keyboard to.
private final class BrowserWebView: WKWebView {
    var onFocus: (() -> Void)?
    /// The app itself is handing over the keyboard.
    var keyboardGranted = false

    override var acceptsFirstResponder: Bool {
        keyboardGranted || BrowserKeyboard.userAsked(by: NSApp.currentEvent, for: self)
    }

    override func becomeFirstResponder() -> Bool {
        let became = super.becomeFirstResponder()
        if became { onFocus?() }
        return became
    }
}

private final class BrowserLocationField: NSTextField {
    var onFocus: (() -> Void)?

    override func becomeFirstResponder() -> Bool {
        let became = super.becomeFirstResponder()
        if became { onFocus?() }
        return became
    }
}

/// A web page in a pane: back, forward, reload and a URL bar over a `WKWebView`.
///
/// A page's reach ends at this view. New windows load here, downloads, camera
/// and microphone are refused, and anything that is not a web page is cancelled
/// rather than handed to macOS, so no page can launch an app.
@MainActor
final class BrowserPaneView: NSView, NSTextFieldDelegate, WKNavigationDelegate, WKUIDelegate {
    /// The pane took the keyboard.
    var onActivate: (() -> Void)?
    /// The page the pane is on changed, for the saved layout and the tab strip.
    var onURLChange: ((String) -> Void)?

    let webView: WKWebView
    private let location = BrowserLocationField()
    private let backButton = NSButton()
    private let forwardButton = NSButton()
    private let reloadButton = NSButton()
    private let message = NSTextField(labelWithString: "")
    private let blankHint = NSTextField(labelWithString: "Type a URL above.")
    private var observations: [NSKeyValueObservation] = []
    /// What the bar goes back to on Esc.
    private(set) var currentURL = ""
    /// Someone is typing in the bar, so the page must not overwrite the text.
    private var barEdited = false

    /// What the pane is telling you, if anything: a refusal or a failed load.
    var messageText: String? { message.isHidden ? nil : message.stringValue }
    var locationText: String { location.stringValue }

    private static let log = Logger(subsystem: "ai.smoo.smoothflow", category: "browser")

    init(url: String) {
        let web = BrowserWebView(frame: .zero, configuration: BrowserWebSetup.configuration())
        webView = web
        super.init(frame: .zero)
        web.onFocus = { [weak self] in self?.onActivate?() }
        location.onFocus = { [weak self] in self?.onActivate?() }
        build()
        observe()
        if case let .ok(parsed) = BrowserURL.parse(url) {
            navigate(to: parsed)
        } else {
            showBlank(true)
        }
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { nil }

    // MARK: navigation

    /// Load `url`, if it is a web page. Everything that opens a page lands here.
    func navigate(to url: URL) {
        guard BrowserURL.isWeb(url) else {
            show(message: BrowserURL.onlyWeb)
            return
        }
        show(message: nil)
        showBlank(false)
        record(url.absoluteString)
        webView.load(URLRequest(url: url))
    }

    /// The bar's Return. Refused text stays in the bar to be fixed. False when
    /// it was refused.
    @discardableResult
    func submit(_ typed: String) -> Bool {
        switch BrowserURL.parse(typed) {
        case let .ok(url):
            barEdited = false
            navigate(to: url)
            giveKeyboardToPage()
            return true
        case let .rejected(why):
            show(message: why)
            NSSound.beep()
            return false
        }
    }

    func goBack() { webView.goBack() }
    func goForward() { webView.goForward() }

    func reload() {
        guard !currentURL.isEmpty else { return }
        webView.reload()
    }

    /// ⌘L: the keyboard in the URL bar, with its text selected.
    func focusLocation() { window?.makeFirstResponder(location) }

    /// The page, or the bar while there is no page.
    func focusContent() {
        if currentURL.isEmpty {
            window?.makeFirstResponder(location)
        } else {
            giveKeyboardToPage()
        }
    }

    private func giveKeyboardToPage() {
        guard let web = webView as? BrowserWebView else { return }
        web.keyboardGranted = true
        window?.makeFirstResponder(web)
        web.keyboardGranted = false
    }

    private func record(_ url: String) {
        if !barEdited { location.stringValue = url }
        guard url != currentURL else { return }
        currentURL = url
        onURLChange?(url)
    }

    private func show(message text: String?) {
        message.stringValue = text ?? ""
        message.isHidden = text == nil
    }

    private func showBlank(_ blank: Bool) {
        blankHint.isHidden = !blank
        webView.isHidden = blank
    }

    // MARK: WKNavigationDelegate

    func webView(_ webView: WKWebView, decidePolicyFor action: WKNavigationAction, decisionHandler: @escaping (WKNavigationActionPolicy) -> Void) {
        let mainFrame = action.targetFrame?.isMainFrame ?? true
        guard let url = action.request.url, BrowserURL.allowsNavigation(url, mainFrame: mainFrame) else {
            Self.log.info("refused navigation to \(action.request.url?.absoluteString ?? "nothing", privacy: .private)")
            if mainFrame { show(message: BrowserURL.onlyWeb) }
            decisionHandler(.cancel)
            return
        }
        decisionHandler(action.shouldPerformDownload ? .cancel : .allow)
    }

    func webView(_ webView: WKWebView, decidePolicyFor response: WKNavigationResponse, decisionHandler: @escaping (WKNavigationResponsePolicy) -> Void) {
        if !response.canShowMIMEType, response.isForMainFrame { show(message: "The browser pane does not download files.") }
        decisionHandler(response.canShowMIMEType ? .allow : .cancel)
    }

    func webView(_ webView: WKWebView, didFailProvisionalNavigation navigation: WKNavigation!, withError error: Error) { failed(error) }

    func webView(_ webView: WKWebView, didFail navigation: WKNavigation!, withError error: Error) { failed(error) }

    func webViewWebContentProcessDidTerminate(_ webView: WKWebView) {
        show(message: "The page crashed and was reloaded.")
        webView.reload()
    }

    private func failed(_ error: Error) {
        let e = error as NSError
        // Our own cancels (a refusal, a new load over the old one) are not failures.
        let cancelled = (e.domain == NSURLErrorDomain && e.code == NSURLErrorCancelled)
            || (e.domain == "WebKitErrorDomain" && e.code == Self.frameLoadInterrupted)
        Self.log.info("load failed: \(e.domain, privacy: .public) \(e.code, privacy: .public)")
        if !cancelled { show(message: e.localizedDescription) }
    }

    /// WebKit's `WebKitErrorFrameLoadInterruptedByPolicyChange`, which it does not export.
    private static let frameLoadInterrupted = 102

    // MARK: WKUIDelegate

    /// `target=_blank` and `window.open` from a click stay in this pane: there is
    /// no second window for a page to open, and script alone is already blocked
    /// by `javaScriptCanOpenWindowsAutomatically`.
    func webView(_ webView: WKWebView, createWebViewWith configuration: WKWebViewConfiguration, for action: WKNavigationAction, windowFeatures: WKWindowFeatures) -> WKWebView? {
        if let url = action.request.url, BrowserURL.isWeb(url) { webView.load(action.request) }
        return nil
    }

    /// The app's camera and microphone grants are for its agents, never a page.
    func webView(_ webView: WKWebView, requestMediaCapturePermissionFor origin: WKSecurityOrigin, initiatedByFrame frame: WKFrameInfo, type: WKMediaCaptureType, decisionHandler: @escaping (WKPermissionDecision) -> Void) {
        decisionHandler(.deny)
    }

    // MARK: NSTextFieldDelegate

    func controlTextDidChange(_ notification: Notification) { barEdited = true }

    func controlTextDidEndEditing(_ notification: Notification) { barEdited = false }

    func control(_ control: NSControl, textView: NSTextView, doCommandBy selector: Selector) -> Bool {
        switch selector {
        case #selector(NSResponder.insertNewline(_:)):
            submit(location.stringValue)
            return true
        case #selector(NSResponder.cancelOperation(_:)):
            barEdited = false
            location.stringValue = currentURL
            show(message: nil)
            focusContent()
            return true
        default:
            return false
        }
    }

    // MARK: building

    private func observe() {
        observations = [
            webView.observe(\.url) { [weak self] web, _ in
                MainActor.assumeIsolated {
                    // Nil and about:blank pass through while a load fails; the
                    // pane keeps saying where it was going.
                    if let url = web.url, BrowserURL.isWeb(url) { self?.record(url.absoluteString) }
                }
            },
            webView.observe(\.canGoBack) { [weak self] web, _ in MainActor.assumeIsolated { self?.backButton.isEnabled = web.canGoBack } },
            webView.observe(\.canGoForward) { [weak self] web, _ in MainActor.assumeIsolated { self?.forwardButton.isEnabled = web.canGoForward } },
            webView.observe(\.isLoading) { [weak self] web, _ in MainActor.assumeIsolated { self?.styleReload(loading: web.isLoading) } },
        ]
    }

    @objc private func backClicked() { goBack() }
    @objc private func forwardClicked() { goForward() }

    @objc private func reloadClicked() {
        if webView.isLoading { webView.stopLoading() } else { reload() }
    }

    private func styleReload(loading: Bool) {
        reloadButton.image = NSImage(systemSymbolName: loading ? "xmark" : "arrow.clockwise", accessibilityDescription: loading ? "Stop" : "Reload")
    }

    private func style(_ button: NSButton, symbol: String, label: String, id: String, action: Selector) {
        button.image = NSImage(systemSymbolName: symbol, accessibilityDescription: label)
        button.isBordered = false
        button.imagePosition = .imageOnly
        button.contentTintColor = Theme.muted
        button.toolTip = label
        button.target = self
        button.action = action
        button.setAccessibilityIdentifier(id)
        button.widthAnchor.constraint(equalToConstant: 22).isActive = true
    }

    private func build() {
        style(backButton, symbol: "chevron.left", label: "Back", id: "browser.back", action: #selector(backClicked))
        style(forwardButton, symbol: "chevron.right", label: "Forward", id: "browser.forward", action: #selector(forwardClicked))
        style(reloadButton, symbol: "arrow.clockwise", label: "Reload", id: "browser.reload", action: #selector(reloadClicked))
        backButton.isEnabled = false
        forwardButton.isEnabled = false

        location.placeholderString = "URL"
        location.font = Theme.monoNSFont(size: 11)
        location.delegate = self
        location.lineBreakMode = .byTruncatingMiddle
        location.cell?.isScrollable = true
        location.setAccessibilityIdentifier("browser.url")
        location.setContentHuggingPriority(.defaultLow, for: .horizontal)

        let bar = NSStackView(views: [backButton, forwardButton, reloadButton, location])
        bar.orientation = .horizontal
        bar.spacing = 4
        bar.edgeInsets = NSEdgeInsets(top: 3, left: 6, bottom: 3, right: 6)

        message.font = .systemFont(ofSize: 11)
        message.textColor = Theme.muted
        message.lineBreakMode = .byTruncatingTail
        message.isHidden = true
        message.setAccessibilityIdentifier("browser.message")

        let content = NSView()
        content.translatesAutoresizingMaskIntoConstraints = false
        content.setContentHuggingPriority(.defaultLow, for: .vertical)
        webView.navigationDelegate = self
        webView.uiDelegate = self
        webView.allowsBackForwardNavigationGestures = true
        webView.translatesAutoresizingMaskIntoConstraints = false
        webView.setAccessibilityIdentifier("browser.page")
        content.addSubview(webView)
        blankHint.font = .systemFont(ofSize: 12)
        blankHint.textColor = Theme.faint
        blankHint.translatesAutoresizingMaskIntoConstraints = false
        content.addSubview(blankHint)

        let column = NSStackView(views: [bar, message, content])
        column.orientation = .vertical
        column.alignment = .leading
        column.spacing = 2
        column.translatesAutoresizingMaskIntoConstraints = false
        addSubview(column)
        setAccessibilityIdentifier("browser.pane")
        NSLayoutConstraint.activate([
            column.leadingAnchor.constraint(equalTo: leadingAnchor), column.trailingAnchor.constraint(equalTo: trailingAnchor),
            column.topAnchor.constraint(equalTo: topAnchor), column.bottomAnchor.constraint(equalTo: bottomAnchor),
            bar.widthAnchor.constraint(equalTo: column.widthAnchor),
            content.widthAnchor.constraint(equalTo: column.widthAnchor),
            webView.leadingAnchor.constraint(equalTo: content.leadingAnchor), webView.trailingAnchor.constraint(equalTo: content.trailingAnchor),
            webView.topAnchor.constraint(equalTo: content.topAnchor), webView.bottomAnchor.constraint(equalTo: content.bottomAnchor),
            blankHint.centerXAnchor.constraint(equalTo: content.centerXAnchor), blankHint.centerYAnchor.constraint(equalTo: content.centerYAnchor),
        ])
    }
}

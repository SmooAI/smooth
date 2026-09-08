import AppKit
import Carbon.HIToolbox
import GhosttyKit

/// An NSView hosting one libghostty surface in MANUAL I/O mode. Output bytes
/// come from the engine (`feed`), typed bytes leave through `onInput`.
/// Forwards keys / text / IME / mouse / scroll / resize / focus / content scale.
final class TerminalSurfaceView: NSView, NSTextInputClient {
    let sessionId: String
    private(set) var surface: ghostty_surface_t?

    /// Bytes the user typed (already terminal-encoded by ghostty) → `flow.input`.
    var onInput: ((Data) -> Void)?
    /// Grid size changed → `flow.resize`.
    var onResize: ((Int, Int) -> Void)?
    var onTitle: ((String) -> Void)?
    var onFocus: ((Bool) -> Void)?

    private var markedText = NSMutableAttributedString()
    private var keyText: [String]?
    private var lastGrid: (cols: Int, rows: Int) = (0, 0)
    private var trackingArea: NSTrackingArea?
    private var cursorShape: NSCursor = .iBeam

    init(sessionId: String) {
        self.sessionId = sessionId
        super.init(frame: NSRect(x: 0, y: 0, width: 400, height: 300))
        wantsLayer = true
        layerContentsRedrawPolicy = .duringViewResize
        createSurface()
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { nil }

    deinit {
        if let surface { ghostty_surface_free(surface) }
    }

    override func makeBackingLayer() -> CALayer {
        let layer = CAMetalLayer()
        layer.pixelFormat = .bgra8Unorm
        layer.isOpaque = true
        layer.framebufferOnly = false
        return layer
    }

    private func createSurface() {
        guard let app = GhosttyRuntime.shared.app else { return }
        var cfg = ghostty_surface_config_new()
        cfg.platform_tag = GHOSTTY_PLATFORM_MACOS
        cfg.platform.macos.nsview = Unmanaged.passUnretained(self).toOpaque()
        cfg.userdata = Unmanaged.passUnretained(self).toOpaque()
        cfg.scale_factor = Double(window?.backingScaleFactor ?? NSScreen.main?.backingScaleFactor ?? 2)
        cfg.context = GHOSTTY_SURFACE_CONTEXT_WINDOW
        cfg.io_mode = GHOSTTY_SURFACE_IO_MANUAL
        cfg.io_write_userdata = Unmanaged.passUnretained(self).toOpaque()
        cfg.io_write_cb = { userdata, bytes, len in
            guard let userdata, let bytes, len > 0 else { return }
            let view = Unmanaged<TerminalSurfaceView>.fromOpaque(userdata).takeUnretainedValue()
            let data = Data(bytes: bytes, count: Int(len))
            // Called on ghostty's I/O thread; hop to main before touching the client.
            DispatchQueue.main.async { view.onInput?(data) }
        }
        surface = ghostty_surface_new(app, &cfg)
    }

    // MARK: output in

    /// `flow.output` → the VT parser. Main thread only (manual-IO contract).
    func feed(_ data: Data) {
        guard let surface, !data.isEmpty else { return }
        data.withUnsafeBytes { raw in
            guard let base = raw.bindMemory(to: CChar.self).baseAddress else { return }
            ghostty_surface_process_output(surface, base, UInt(raw.count))
        }
    }

    var gridSize: (cols: Int, rows: Int) {
        guard let surface else { return (80, 24) }
        let s = ghostty_surface_size(surface)
        return (Int(s.columns), Int(s.rows))
    }

    // MARK: geometry / focus

    override var acceptsFirstResponder: Bool { true }

    override func viewDidMoveToWindow() {
        super.viewDidMoveToWindow()
        updateScale()
        updateSize()
    }

    override func viewDidChangeBackingProperties() {
        super.viewDidChangeBackingProperties()
        updateScale()
        updateSize()
    }

    override func setFrameSize(_ newSize: NSSize) {
        super.setFrameSize(newSize)
        updateSize()
    }

    override func layout() {
        super.layout()
        updateSize()
    }

    private func updateScale() {
        guard let surface else { return }
        let scale = window?.backingScaleFactor ?? 2
        (layer as? CAMetalLayer)?.contentsScale = scale
        ghostty_surface_set_content_scale(surface, scale, scale)
    }

    private func updateSize() {
        guard let surface, bounds.width > 0, bounds.height > 0 else { return }
        let scale = window?.backingScaleFactor ?? 2
        ghostty_surface_set_size(surface, UInt32(bounds.width * scale), UInt32(bounds.height * scale))
        // Ghostty applies the size on its own thread, so the grid read right
        // here is still the OLD one (a window resize never reached the engine
        // as `flow.resize` — measured: cols stuck at the first attach). Check
        // now and again shortly after; only a real change is reported.
        reportGridIfChanged()
        DispatchQueue.main.asyncAfter(deadline: .now() + .milliseconds(80)) { [weak self] in self?.reportGridIfChanged() }
        DispatchQueue.main.asyncAfter(deadline: .now() + .milliseconds(300)) { [weak self] in self?.reportGridIfChanged() }
    }

    private func reportGridIfChanged() {
        let g = gridSize
        if g != lastGrid, g.cols > 0, g.rows > 0 {
            lastGrid = g
            onResize?(g.cols, g.rows)
        }
    }

    override func becomeFirstResponder() -> Bool {
        let ok = super.becomeFirstResponder()
        if ok, let surface { ghostty_surface_set_focus(surface, true); onFocus?(true) }
        return ok
    }

    override func resignFirstResponder() -> Bool {
        let ok = super.resignFirstResponder()
        if ok, let surface { ghostty_surface_set_focus(surface, false); onFocus?(false) }
        return ok
    }

    override func updateTrackingAreas() {
        if let trackingArea { removeTrackingArea(trackingArea) }
        let ta = NSTrackingArea(rect: bounds, options: [.mouseMoved, .mouseEnteredAndExited, .activeInKeyWindow, .inVisibleRect], owner: self)
        addTrackingArea(ta)
        trackingArea = ta
        super.updateTrackingAreas()
    }

    override func resetCursorRects() { addCursorRect(bounds, cursor: cursorShape) }

    func setMouseShape(_ shape: ghostty_action_mouse_shape_e) {
        switch shape {
        case GHOSTTY_MOUSE_SHAPE_TEXT: cursorShape = .iBeam
        case GHOSTTY_MOUSE_SHAPE_POINTER: cursorShape = .pointingHand
        case GHOSTTY_MOUSE_SHAPE_CROSSHAIR: cursorShape = .crosshair
        default: cursorShape = .arrow
        }
        window?.invalidateCursorRects(for: self)
    }

    // MARK: keyboard

    override func keyDown(with event: NSEvent) {
        guard let surface else { return }
        keyText = []
        // Runs the input method: insertText / setMarkedText land below.
        interpretKeyEvents([event])
        let text = keyText?.joined() ?? ""
        keyText = nil

        var key = ghostty_input_key_s()
        key.action = event.isARepeat ? GHOSTTY_ACTION_REPEAT : GHOSTTY_ACTION_PRESS
        key.mods = Self.mods(event.modifierFlags)
        key.keycode = UInt32(event.keyCode)
        key.unshifted_codepoint = event.charactersIgnoringModifiers?.unicodeScalars.first?.value ?? 0
        key.composing = markedText.length > 0
        if !text.isEmpty {
            // The input method consumed shift/alt to produce `text`; ctrl/cmd stay.
            key.consumed_mods = ghostty_input_mods_e(key.mods.rawValue & (GHOSTTY_MODS_SHIFT.rawValue | GHOSTTY_MODS_ALT.rawValue))
            text.withCString { ptr in
                key.text = ptr
                _ = ghostty_surface_key(surface, key)
            }
        } else {
            _ = ghostty_surface_key(surface, key)
        }
    }

    override func keyUp(with event: NSEvent) {
        guard let surface else { return }
        var key = ghostty_input_key_s()
        key.action = GHOSTTY_ACTION_RELEASE
        key.mods = Self.mods(event.modifierFlags)
        key.keycode = UInt32(event.keyCode)
        key.unshifted_codepoint = event.charactersIgnoringModifiers?.unicodeScalars.first?.value ?? 0
        _ = ghostty_surface_key(surface, key)
    }

    override func flagsChanged(with event: NSEvent) {
        guard let surface else { return }
        let mods = Self.mods(event.modifierFlags)
        let pressed: Bool
        switch Int(event.keyCode) {
        case kVK_Shift, kVK_RightShift: pressed = event.modifierFlags.contains(.shift)
        case kVK_Control, kVK_RightControl: pressed = event.modifierFlags.contains(.control)
        case kVK_Option, kVK_RightOption: pressed = event.modifierFlags.contains(.option)
        case kVK_Command, kVK_RightCommand: pressed = event.modifierFlags.contains(.command)
        case kVK_CapsLock: pressed = event.modifierFlags.contains(.capsLock)
        default: return
        }
        var key = ghostty_input_key_s()
        key.action = pressed ? GHOSTTY_ACTION_PRESS : GHOSTTY_ACTION_RELEASE
        key.mods = mods
        key.keycode = UInt32(event.keyCode)
        _ = ghostty_surface_key(surface, key)
    }

    static func mods(_ flags: NSEvent.ModifierFlags) -> ghostty_input_mods_e {
        var m: UInt32 = 0
        if flags.contains(.shift) { m |= GHOSTTY_MODS_SHIFT.rawValue }
        if flags.contains(.control) { m |= GHOSTTY_MODS_CTRL.rawValue }
        if flags.contains(.option) { m |= GHOSTTY_MODS_ALT.rawValue }
        if flags.contains(.command) { m |= GHOSTTY_MODS_SUPER.rawValue }
        if flags.contains(.capsLock) { m |= GHOSTTY_MODS_CAPS.rawValue }
        return ghostty_input_mods_e(m)
    }

    // MARK: NSTextInputClient (IME + plain text)

    func insertText(_ string: Any, replacementRange: NSRange) {
        let s = (string as? NSAttributedString)?.string ?? (string as? String) ?? ""
        if markedText.length > 0 { markedText = NSMutableAttributedString(); surface.map { ghostty_surface_preedit($0, nil, 0) } }
        if keyText != nil {
            keyText?.append(s)
        } else if let surface, !s.isEmpty {
            // Text arriving outside keyDown (dictation, emoji picker).
            s.withCString { ghostty_surface_text(surface, $0, UInt(s.utf8.count)) }
        }
    }

    func setMarkedText(_ string: Any, selectedRange: NSRange, replacementRange: NSRange) {
        let s = (string as? NSAttributedString)?.string ?? (string as? String) ?? ""
        markedText = NSMutableAttributedString(string: s)
        guard let surface else { return }
        s.withCString { ghostty_surface_preedit(surface, $0, UInt(s.utf8.count)) }
    }

    func unmarkText() {
        markedText = NSMutableAttributedString()
        surface.map { ghostty_surface_preedit($0, nil, 0) }
    }

    func selectedRange() -> NSRange { NSRange(location: NSNotFound, length: 0) }
    func markedRange() -> NSRange { markedText.length > 0 ? NSRange(location: 0, length: markedText.length) : NSRange(location: NSNotFound, length: 0) }
    func hasMarkedText() -> Bool { markedText.length > 0 }
    func attributedSubstring(forProposedRange range: NSRange, actualRange: NSRangePointer?) -> NSAttributedString? { nil }
    func validAttributesForMarkedText() -> [NSAttributedString.Key] { [] }
    func characterIndex(for point: NSPoint) -> Int { 0 }

    func firstRect(forCharacterRange range: NSRange, actualRange: NSRangePointer?) -> NSRect {
        guard let surface, let window else { return .zero }
        var x = 0.0, y = 0.0, w = 0.0, h = 0.0
        ghostty_surface_ime_point(surface, &x, &y, &w, &h)
        let local = NSRect(x: x, y: bounds.height - y - h, width: w, height: h)
        return window.convertToScreen(convert(local, to: nil))
    }

    override func doCommand(by selector: Selector) {
        // Arrow keys, delete, etc. reach ghostty via keyDown's keycode; nothing to do.
    }

    // MARK: mouse

    private func surfacePoint(_ event: NSEvent) -> NSPoint {
        let p = convert(event.locationInWindow, from: nil)
        return NSPoint(x: p.x, y: bounds.height - p.y)
    }

    private func button(_ event: NSEvent, _ state: ghostty_input_mouse_state_e) {
        guard let surface else { return }
        let b: ghostty_input_mouse_button_e = switch event.buttonNumber {
        case 0: GHOSTTY_MOUSE_LEFT
        case 1: GHOSTTY_MOUSE_RIGHT
        case 2: GHOSTTY_MOUSE_MIDDLE
        default: GHOSTTY_MOUSE_UNKNOWN
        }
        let p = surfacePoint(event)
        ghostty_surface_mouse_pos(surface, p.x, p.y, Self.mods(event.modifierFlags))
        _ = ghostty_surface_mouse_button(surface, state, b, Self.mods(event.modifierFlags))
    }

    override func mouseDown(with event: NSEvent) {
        window?.makeFirstResponder(self)
        button(event, GHOSTTY_MOUSE_PRESS)
    }
    override func mouseUp(with event: NSEvent) { button(event, GHOSTTY_MOUSE_RELEASE) }
    override func rightMouseDown(with event: NSEvent) { button(event, GHOSTTY_MOUSE_PRESS) }
    override func rightMouseUp(with event: NSEvent) { button(event, GHOSTTY_MOUSE_RELEASE) }
    override func otherMouseDown(with event: NSEvent) { button(event, GHOSTTY_MOUSE_PRESS) }
    override func otherMouseUp(with event: NSEvent) { button(event, GHOSTTY_MOUSE_RELEASE) }

    private func moved(_ event: NSEvent) {
        guard let surface else { return }
        let p = surfacePoint(event)
        ghostty_surface_mouse_pos(surface, p.x, p.y, Self.mods(event.modifierFlags))
    }

    override func mouseMoved(with event: NSEvent) { moved(event) }
    override func mouseDragged(with event: NSEvent) { moved(event) }
    override func rightMouseDragged(with event: NSEvent) { moved(event) }
    override func mouseExited(with event: NSEvent) {
        guard let surface else { return }
        ghostty_surface_mouse_pos(surface, -1, -1, Self.mods(event.modifierFlags))
    }

    override func scrollWheel(with event: NSEvent) {
        guard let surface else { return }
        var x = event.scrollingDeltaX
        var y = event.scrollingDeltaY
        var mods: Int32 = 0
        if event.hasPreciseScrollingDeltas {
            mods |= 1
        } else {
            // Legacy wheels report lines; ghostty wants pixels.
            x *= 10; y *= 10
        }
        let momentum: Int32 = switch event.momentumPhase {
        case .began: 1
        case .stationary: 2
        case .changed: 3
        case .ended: 4
        case .cancelled: 5
        case .mayBegin: 6
        default: 0
        }
        mods |= momentum << 1
        ghostty_surface_mouse_scroll(surface, x, y, ghostty_input_scroll_mods_t(mods))
    }

    // MARK: drawing

    override func updateLayer() {
        guard let surface else { return }
        ghostty_surface_draw(surface)
    }

    override var isOpaque: Bool { true }
}

import AppKit
import GhosttyKit

/// One `ghostty_app_t` for the process. Surfaces are created in MANUAL I/O
/// mode: ghostty spawns nothing and owns no PTY — the engine's `flow.output`
/// bytes go in through `ghostty_surface_process_output`, and what the user
/// types comes back out of the io_write callback as `flow.input`.
@MainActor
final class GhosttyRuntime {
    static let shared = GhosttyRuntime()

    private(set) var app: ghostty_app_t?
    private(set) var config: ghostty_config_t?
    private var tickScheduled = false

    /// Set by the surface view registry so action callbacks can find their view.
    private init() {
        // ghostty_init reads argv for CLI actions; we have none.
        var argv: [UnsafeMutablePointer<CChar>?] = [strdup("smoothflow"), nil]
        defer { free(argv[0]) }
        argv.withUnsafeMutableBufferPointer { buf in
            _ = ghostty_init(UInt(buf.count - 1), buf.baseAddress)
        }

        guard let cfg = ghostty_config_new() else { return }
        // The user's own ~/.config/ghostty config applies (fonts, theme), then
        // our overrides: no shell integration (no PTY of ours to integrate with).
        ghostty_config_load_default_files(cfg)
        let overrides = "shell-integration = none\nconfirm-close-surface = false\nwindow-padding-x = 6\nwindow-padding-y = 4\n"
        overrides.withCString { ov in "smoothflow".withCString { src in ghostty_config_load_string(cfg, ov, UInt(overrides.utf8.count), src) } }
        ghostty_config_finalize(cfg)
        config = cfg

        var rt = ghostty_runtime_config_s()
        rt.userdata = Unmanaged.passUnretained(self).toOpaque()
        rt.supports_selection_clipboard = false
        rt.wakeup_cb = { userdata in
            guard let userdata else { return }
            let me = Unmanaged<GhosttyRuntime>.fromOpaque(userdata).takeUnretainedValue()
            DispatchQueue.main.async { me.tick() }
        }
        rt.action_cb = { app, target, action in
            // Ghostty raises actions from its renderer/IO threads too (a full
            // redraw from the real engine does it on the first attach); the
            // mock never tripped this. Off-main, hop for anything that touches
            // a view and acknowledge the rest — `assumeIsolated` there is a trap.
            if Thread.isMainThread {
                return MainActor.assumeIsolated { GhosttyRuntime.handleAction(app: app, target: target, action: action) }
            }
            return GhosttyRuntime.handleActionOffMain(target: target, action: action)
        }
        rt.read_clipboard_cb = { userdata, _, state in
            // Usually the app tick (main), but the same renderer/IO threads that
            // raise actions can reach here — `assumeIsolated` off-main is the
            // SIGTRAP in the 2026-09-08 crash report. Hop instead of asserting.
            GhosttyRuntime.onMain { GhosttyRuntime.completeClipboardRead(surfaceUserdata: userdata, state: state) }
            return true
        }
        rt.confirm_read_clipboard_cb = { userdata, _, state, _ in
            GhosttyRuntime.onMain { GhosttyRuntime.completeClipboardRead(surfaceUserdata: userdata, state: state) }
        }
        rt.write_clipboard_cb = { _, _, contents, count, _ in
            guard let contents, count > 0 else { return }
            for i in 0..<Int(count) {
                let c = contents[i]
                guard let mime = c.mime, let data = c.data, String(cString: mime).hasPrefix("text/plain") else { continue }
                let s = String(cString: data)
                DispatchQueue.main.async {
                    NSPasteboard.general.clearContents()
                    NSPasteboard.general.setString(s, forType: .string)
                }
                return
            }
        }
        rt.close_surface_cb = { _, _ in }
        app = ghostty_app_new(&rt, cfg)
    }

    func tick() {
        guard let app else { return }
        ghostty_app_tick(app)
    }

    func setFocus(_ focused: Bool) {
        guard let app else { return }
        ghostty_app_set_focus(app, focused)
    }

    private static func completeClipboardRead(surfaceUserdata: UnsafeMutableRawPointer?, state: UnsafeMutableRawPointer?) {
        guard let surfaceUserdata else { return }
        let view = Unmanaged<TerminalSurfaceView>.fromOpaque(surfaceUserdata).takeUnretainedValue()
        guard let surface = view.surface else { return }
        let text = NSPasteboard.general.string(forType: .string) ?? ""
        text.withCString { ghostty_surface_complete_clipboard_request(surface, $0, state, false) }
    }

    /// The subset of `handleAction` that is safe from a ghostty background
    /// thread: copy what the action carries, then dispatch to the main actor.
    /// Run `body` on the main actor: inline when already there, else async.
    nonisolated static func onMain(_ body: @escaping @MainActor () -> Void) {
        if Thread.isMainThread {
            MainActor.assumeIsolated(body)
        } else {
            DispatchQueue.main.async { body() }
        }
    }

    nonisolated private static func handleActionOffMain(target: ghostty_target_s, action: ghostty_action_s) -> Bool {
        guard target.tag == GHOSTTY_TARGET_SURFACE, let surface = target.target.surface,
              let ud = ghostty_surface_userdata(surface) else { return false }
        let view = Unmanaged<TerminalSurfaceView>.fromOpaque(ud).takeUnretainedValue()
        switch action.tag {
        case GHOSTTY_ACTION_SET_TITLE, GHOSTTY_ACTION_SET_TAB_TITLE:
            guard let t = action.action.set_title.title else { return true }
            let title = String(cString: t)
            DispatchQueue.main.async { view.onTitle?(title) }
            return true
        case GHOSTTY_ACTION_MOUSE_SHAPE:
            let shape = action.action.mouse_shape
            DispatchQueue.main.async { view.setMouseShape(shape) }
            return true
        case GHOSTTY_ACTION_RING_BELL:
            DispatchQueue.main.async { NSSound.beep() }
            return true
        case GHOSTTY_ACTION_CELL_SIZE, GHOSTTY_ACTION_RENDER, GHOSTTY_ACTION_MOUSE_VISIBILITY, GHOSTTY_ACTION_PWD,
             GHOSTTY_ACTION_MOUSE_OVER_LINK, GHOSTTY_ACTION_COLOR_CHANGE, GHOSTTY_ACTION_SCROLLBAR, GHOSTTY_ACTION_RENDERER_HEALTH,
             GHOSTTY_ACTION_PROGRESS_REPORT, GHOSTTY_ACTION_SELECTION_CHANGED:
            return true
        default:
            return false
        }
    }

    private static func handleAction(app: ghostty_app_t?, target: ghostty_target_s, action: ghostty_action_s) -> Bool {
        guard target.tag == GHOSTTY_TARGET_SURFACE, let surface = target.target.surface,
              let ud = ghostty_surface_userdata(surface) else { return false }
        let view = Unmanaged<TerminalSurfaceView>.fromOpaque(ud).takeUnretainedValue()
        switch action.tag {
        case GHOSTTY_ACTION_SET_TITLE, GHOSTTY_ACTION_SET_TAB_TITLE:
            if let t = action.action.set_title.title { view.onTitle?(String(cString: t)) }
            return true
        case GHOSTTY_ACTION_MOUSE_SHAPE:
            view.setMouseShape(action.action.mouse_shape)
            return true
        case GHOSTTY_ACTION_RING_BELL:
            NSSound.beep()
            return true
        case GHOSTTY_ACTION_CELL_SIZE, GHOSTTY_ACTION_RENDER, GHOSTTY_ACTION_MOUSE_VISIBILITY, GHOSTTY_ACTION_PWD,
             GHOSTTY_ACTION_MOUSE_OVER_LINK, GHOSTTY_ACTION_COLOR_CHANGE, GHOSTTY_ACTION_SCROLLBAR, GHOSTTY_ACTION_RENDERER_HEALTH,
             GHOSTTY_ACTION_PROGRESS_REPORT, GHOSTTY_ACTION_SELECTION_CHANGED:
            return true
        default:
            return false
        }
    }
}

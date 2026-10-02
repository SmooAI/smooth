//! SmoothFlow for Linux and Windows (th-3e6020): the agent fleet console with
//! GPU-rendered terminals, built on GPUI from the SmoothFlow Client Spec
//! (docs/Architecture/SmoothFlow-Client-Spec.md). M2: the fleet sidebar, tabs
//! and splits of live terminals, New Session, and approvals.

mod view;
mod workspace;

use gpui_kit::*;
use smoothflow_desktop::net;

fn main() {
    let smooth_dir = dirs_next::home_dir().unwrap_or_default().join(".smooth");
    let (out, events) = net::start(smooth_dir);
    let keymap = workspace::load_keymap();
    gpui_kit::application().run(move |cx: &mut App| {
        gpui_kit::init(cx);
        let bounds = Bounds::centered(None, size(px(1280.0), px(820.0)), cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions {
                title: Some("SmoothFlow".into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let opened = cx.open_window(options, move |window, cx| {
            let view = cx.new(|cx| workspace::Workspace::new(out, events, keymap, cx));
            view.read(cx).focus_handle().clone().focus(window, cx);
            view
        });
        if opened.is_err() {
            eprintln!("smoothflow: could not open a window");
            cx.quit();
            return;
        }
        cx.activate(true);
    });
}

//! The window: a GPUI entity around the toolkit-free [`Core`]. Everything the
//! window *does* lives in `smoothflow_desktop::app_core` (so `tests/e2e.rs`
//! can drive it against a real daemon); this adds only what needs GPUI —
//! focus, font metrics, repaints, and running the sheet's HTTP reads on a
//! background thread. The `view` module draws.

use std::ops::{Deref, DerefMut};

use gpui_kit::*;
use smooth_flow_client::keymap::{Action, Keymap, Platform};

use smoothflow_desktop::app_core::{Confirmed, Core, Fetch};
use smoothflow_desktop::keys;
use smoothflow_desktop::layout::CellMetrics;
use smoothflow_desktop::net::{Event, Outbox};
use smoothflow_desktop::sheet::Effect;

pub use smoothflow_desktop::app_core::Connection;

pub struct Workspace {
    pub(crate) focus: FocusHandle,
    pub(crate) metrics: Option<CellMetrics>,
    /// The Diff body's virtualized list.
    pub(crate) diff_scroll: UniformListScrollHandle,
    /// The pane area's top-left in the window (sidebar and bars above it).
    pub(crate) pane_origin: (f32, f32),
    core: Core,
}

/// The view reads the core's state as if it were the workspace's own.
impl Deref for Workspace {
    type Target = Core;
    fn deref(&self) -> &Core {
        &self.core
    }
}

impl DerefMut for Workspace {
    fn deref_mut(&mut self) -> &mut Core {
        &mut self.core
    }
}

impl Workspace {
    pub fn new(out: Outbox, events: futures::channel::mpsc::UnboundedReceiver<Event>, keymap: Keymap, cx: &mut Context<Self>) -> Self {
        cx.spawn(async move |this, cx| {
            use futures::StreamExt;
            let mut events = events;
            while let Some(ev) = events.next().await {
                if this
                    .update(cx, |ws, cx| {
                        ws.core.apply(ev);
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
        let home = dirs_next::home_dir().map(|h| h.to_string_lossy().into_owned()).unwrap_or_default();
        Self {
            focus: cx.focus_handle(),
            metrics: None,
            diff_scroll: UniformListScrollHandle::new(),
            pane_origin: (0.0, 0.0),
            core: Core::new(out, keymap, home),
        }
    }

    pub fn focus_handle(&self) -> &FocusHandle {
        &self.focus
    }

    /// Run the sheet's HTTP reads off the UI thread, then repaint.
    fn fetch(&mut self, fetches: Vec<Fetch>, cx: &mut Context<Self>) {
        for f in fetches {
            cx.spawn(async move |this, cx| {
                let loaded = cx.background_spawn(async move { f.run() }).await;
                this.update(cx, |ws, cx| {
                    ws.core.loaded(loaded);
                    cx.notify();
                })
                .ok();
            })
            .detach();
        }
        cx.notify();
    }

    pub(crate) fn on_key(&mut self, ev: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let k = &ev.keystroke;
        let key = keys::Key {
            key: &k.key,
            key_char: k.key_char.as_deref(),
            control: k.modifiers.control,
            alt: k.modifiers.alt,
            shift: k.modifiers.shift,
            platform: k.modifiers.platform,
        };
        let fetches = self.core.key(key);
        self.fetch(fetches, cx);
    }

    /// Run a keymap action (also what the buttons call).
    pub(crate) fn act(&mut self, action: Action, cx: &mut Context<Self>) {
        let fetches = self.core.act(action);
        self.fetch(fetches, cx);
    }

    pub(crate) fn run_effects(&mut self, effects: Vec<Effect>, cx: &mut Context<Self>) {
        let fetches = self.core.run_effects(effects);
        self.fetch(fetches, cx);
    }

    /// Ask before reverting a hunk (its Revert button).
    pub(crate) fn ask_revert(&mut self, ask: smoothflow_desktop::diff::RevertAsk) {
        self.core.ask_revert(ask);
    }

    /// A dialog button.
    pub(crate) fn confirm(&mut self, c: Confirmed, cx: &mut Context<Self>) {
        self.core.confirm(c);
        cx.notify();
    }

    /// Close Out a fleet row's session (middle-click): asks first, like
    /// the `closeOut` action.
    pub(crate) fn close_out(&mut self, id: &str, cx: &mut Context<Self>) {
        self.core.close_out(id);
        cx.notify();
    }

    pub(crate) fn dont_ask_again(&mut self, cx: &mut Context<Self>) {
        self.core.dont_ask_again();
        cx.notify();
    }
}

/// The user's keymap: `~/.smooth/smoothflow/keybindings.toml` over the
/// platform defaults (spec §9). Problems are printed, never fatal.
pub fn load_keymap() -> Keymap {
    let path = dirs_next::home_dir().unwrap_or_default().join(".smooth/smoothflow/keybindings.toml");
    let map = std::fs::read_to_string(&path).map_or_else(|_| Keymap::defaults(Platform::current()), |t| Keymap::parse(&t, Platform::current()));
    for p in &map.problems {
        eprintln!("smoothflow: {}: {p}", path.display());
    }
    for (chord, actions) in map.conflicts() {
        let names: Vec<&str> = actions.iter().map(|a| a.name()).collect();
        eprintln!("smoothflow: {} is bound to {} — the first wins", chord.wire(), names.join(", "));
    }
    map
}

//! A window's tabs (spec §5): tabs hold panes, panes show sessions, and a
//! close collapses pane → tab → last, where the last pane empties and the
//! window stays.
//!
//! [`crate::pane::Tab`] is one tab's layout; this is the strip of them, with
//! pane ids unique across the whole window so a view can key a surface by id.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::close::Scope;
use crate::pane::{Direction, PaneId, Rect, Tab};

/// Every tab of a window, and which is active.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Surfaces {
    pub tabs: Vec<Tab>,
    pub active: usize,
    next_pane: PaneId,
}

impl Default for Surfaces {
    fn default() -> Self {
        Self::new()
    }
}

impl Surfaces {
    /// One tab of one empty pane (pane 1).
    #[must_use]
    pub fn new() -> Self {
        Self {
            tabs: vec![Tab::new(1, None)],
            active: 0,
            next_pane: 2,
        }
    }

    fn alloc(&mut self) -> PaneId {
        let id = self.next_pane;
        self.next_pane += 1;
        id
    }

    /// The active tab.
    #[must_use]
    pub fn tab(&self) -> &Tab {
        &self.tabs[self.active.min(self.tabs.len() - 1)]
    }

    fn tab_mut(&mut self) -> &mut Tab {
        let i = self.active.min(self.tabs.len() - 1);
        &mut self.tabs[i]
    }

    /// The focused pane of the active tab.
    #[must_use]
    pub fn focused_pane(&self) -> PaneId {
        self.tab().focused
    }

    /// The session the focused pane shows.
    #[must_use]
    pub fn focused_session(&self) -> Option<&str> {
        let t = self.tab();
        t.sessions.get(&t.focused).map(String::as_str)
    }

    /// The focused pane shows `session` (clicking a fleet row).
    pub fn show(&mut self, session: &str) {
        let t = self.tab_mut();
        let f = t.focused;
        t.sessions.insert(f, session.to_string());
    }

    /// Focus a pane of the active tab by id (a click); ignored when not there.
    pub fn focus_pane(&mut self, pane: PaneId) {
        let t = self.tab_mut();
        if t.root.contains(pane) && t.zoomed.is_none_or(|z| z == pane) {
            t.focused = pane;
        }
    }

    /// A new tab right after the active one, active, showing `session`.
    pub fn new_tab(&mut self, session: Option<&str>) {
        let id = self.alloc();
        let at = (self.active + 1).min(self.tabs.len());
        self.tabs.insert(at, Tab::new(id, session));
        self.active = at;
    }

    /// Make tab `i` active; ignored when out of range.
    pub fn select(&mut self, i: usize) {
        if i < self.tabs.len() {
            self.active = i;
        }
    }

    /// Previous (`-1`) / next (`+1`) tab, wrapping.
    pub fn cycle(&mut self, delta: isize) {
        let n = isize::try_from(self.tabs.len()).unwrap_or(1).max(1);
        let cur = isize::try_from(self.active).unwrap_or(0);
        self.active = usize::try_from((cur + delta).rem_euclid(n)).unwrap_or(0);
    }

    /// Split the focused pane; the new pane shows the same session.
    pub fn split(&mut self, direction: Direction) {
        let id = self.alloc();
        self.tab_mut().split(direction, id);
    }

    pub fn focus(&mut self, direction: Direction, bounds: Rect) {
        self.tab_mut().focus(direction, bounds);
    }

    pub fn toggle_zoom(&mut self) {
        self.tab_mut().toggle_zoom();
    }

    pub fn equalize(&mut self) {
        self.tab_mut().equalize();
    }

    /// What `closePane` would take with it now.
    #[must_use]
    pub fn close_scope(&self) -> Scope {
        Scope::of(self.tab().panes().len(), self.tabs.len())
    }

    /// Some pane other than the focused one — in any tab — shows the focused
    /// pane's session, so closing this view loses nothing.
    #[must_use]
    pub fn shown_elsewhere(&self) -> bool {
        let Some(s) = self.focused_session() else { return false };
        let (active, focused) = (self.active, self.focused_pane());
        self.tabs
            .iter()
            .enumerate()
            .any(|(i, t)| t.sessions.iter().any(|(p, x)| x == s && !(i == active && *p == focused)))
    }

    /// Sessions the active tab shows that no other tab shows (what closing
    /// the whole tab could lose sight of).
    #[must_use]
    pub fn tab_only_sessions(&self) -> Vec<String> {
        let mine: BTreeSet<&String> = self.tab().sessions.values().collect();
        let others: BTreeSet<&String> = self
            .tabs
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != self.active)
            .flat_map(|(_, t)| t.sessions.values())
            .collect();
        mine.difference(&others).map(|s| (*s).clone()).collect()
    }

    /// `closePane`: the focused pane goes; a tab's last pane takes the tab;
    /// the last pane of the last tab empties. Returns the scope applied.
    pub fn close_pane(&mut self) -> Scope {
        let scope = self.close_scope();
        match scope {
            Scope::Pane => {
                self.tab_mut().close_focused();
            }
            Scope::Tab => self.remove_active_tab(),
            Scope::Last => {
                let t = self.tab_mut();
                let f = t.focused;
                t.sessions.remove(&f);
            }
        }
        scope
    }

    /// `closeTab`: the whole tab, splits and all. The last tab resets to one
    /// empty pane — the window stays. Returns the scope applied (`tab`, or
    /// `last` for the reset).
    pub fn close_tab(&mut self) -> Scope {
        if self.tabs.len() > 1 {
            self.remove_active_tab();
            Scope::Tab
        } else {
            let id = self.alloc();
            self.tabs[0] = Tab::new(id, None);
            self.active = 0;
            Scope::Last
        }
    }

    /// Focus moves to the tab before (or after, for the first), as panes do.
    fn remove_active_tab(&mut self) {
        let i = self.active.min(self.tabs.len() - 1);
        self.tabs.remove(i);
        self.active = i.saturating_sub(1).min(self.tabs.len() - 1);
    }

    /// A session went away: every pane showing it empties.
    pub fn forget(&mut self, session: &str) {
        for t in &mut self.tabs {
            t.sessions.retain(|_, s| s != session);
        }
    }

    /// The panes on screen in the active tab and their frames in `bounds`:
    /// just the zoomed pane, filling the tab, while one is zoomed.
    #[must_use]
    pub fn visible(&self, bounds: Rect) -> Vec<(PaneId, Rect)> {
        let t = self.tab();
        if let Some(z) = t.zoomed.filter(|z| t.root.contains(*z)) {
            return vec![(z, bounds)];
        }
        t.root.frames(bounds).into_iter().collect()
    }

    /// The session pane `pane` of the active tab shows.
    #[must_use]
    pub fn session_of(&self, pane: PaneId) -> Option<&str> {
        self.tab().sessions.get(&pane).map(String::as_str)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const B: Rect = Rect {
        x: 0.0,
        y: 0.0,
        w: 100.0,
        h: 100.0,
    };

    #[test]
    fn close_collapses_pane_then_tab_then_empties_the_last() {
        let mut s = Surfaces::new();
        s.show("fs-a");
        s.split(Direction::Right);
        assert!(s.shown_elsewhere(), "the split shows the same session");
        assert_eq!(s.close_pane(), Scope::Pane);
        s.new_tab(Some("fs-b"));
        assert_eq!((s.tabs.len(), s.active), (2, 1));
        assert!(!s.shown_elsewhere());
        assert_eq!(s.close_pane(), Scope::Tab);
        assert_eq!((s.tabs.len(), s.active), (1, 0));
        assert_eq!(s.focused_session(), Some("fs-a"));
        assert_eq!(s.close_pane(), Scope::Last);
        assert_eq!((s.tabs.len(), s.focused_session()), (1, None), "the window stays, the pane is empty");
        assert_eq!(s.close_pane(), Scope::Last, "closing an empty last pane is harmless");
    }

    #[test]
    fn tabs_cycle_and_close_whole() {
        let mut s = Surfaces::new();
        s.new_tab(Some("a"));
        s.new_tab(Some("b"));
        assert_eq!(s.active, 2);
        s.cycle(1);
        assert_eq!(s.active, 0, "wraps");
        s.cycle(-1);
        assert_eq!(s.active, 2);
        s.split(Direction::Down);
        assert_eq!(s.tab_only_sessions(), vec!["b".to_string()]);
        assert_eq!(s.close_tab(), Scope::Tab);
        assert_eq!((s.tabs.len(), s.active), (2, 1));
        s.select(0);
        s.close_tab();
        s.close_tab();
        assert_eq!(s.tabs.len(), 1);
        assert_eq!(s.tab().panes().len(), 1);
        assert_eq!(s.focused_session(), None);
    }

    #[test]
    fn pane_ids_are_unique_across_tabs_and_zoom_shows_one() {
        let mut s = Surfaces::new();
        s.show("a");
        s.split(Direction::Right);
        s.new_tab(Some("a"));
        s.split(Direction::Down);
        let ids: Vec<PaneId> = s.tabs.iter().flat_map(Tab::panes).collect();
        assert_eq!(ids, vec![1, 2, 3, 4]);
        assert_eq!(s.visible(B).len(), 2);
        s.toggle_zoom();
        assert_eq!(s.visible(B), vec![(4, B)]);
        s.focus_pane(3);
        assert_eq!(s.focused_pane(), 4, "a zoomed tab keeps its focus");
        s.forget("a");
        assert!(s.tabs.iter().all(|t| t.sessions.is_empty()));
    }
}

//! The pane layout of one tab (spec §5): a binary tree whose leaves are panes.
//!
//! A tree, not a list, because "split the focused pane downward" has no meaning
//! in a flat row. Leaves carry an id so a view can keep a surface alive across a
//! relayout, and each split carries a fraction so a dragged divider survives.
//!
//! Geometry here is top-down (y grows downward, as GPUI and most toolkits
//! draw). AppKit is bottom-up; the Mac app converts. What the spec pins —
//! which pane a direction reaches — is the same either way.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// A pane's identity: opaque and monotonic, never an index, so removing one
/// pane can't silently retarget another's surface.
pub type PaneId = u32;

/// Where a new pane goes relative to the focused one, or which way focus moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

impl Direction {
    /// A left/right split lays panes side by side.
    #[must_use]
    pub const fn is_horizontal(self) -> bool {
        matches!(self, Self::Left | Self::Right)
    }

    /// The new pane comes after the existing one in layout order.
    #[must_use]
    pub const fn inserts_after(self) -> bool {
        matches!(self, Self::Right | Self::Down)
    }
}

/// A node of the layout tree.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Node {
    Leaf {
        pane: PaneId,
    },
    Split {
        horizontal: bool,
        first: Box<Self>,
        second: Box<Self>,
        /// The share of the axis `first` takes.
        fraction: f64,
    },
}

/// A rectangle, top-down.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Rect {
    fn mid_x(self) -> f64 {
        self.x + self.w / 2.0
    }
    fn mid_y(self) -> f64 {
        self.y + self.h / 2.0
    }
    fn max_x(self) -> f64 {
        self.x + self.w
    }
    fn max_y(self) -> f64 {
        self.y + self.h
    }
}

impl Node {
    /// The panes, in layout order.
    #[must_use]
    pub fn leaves(&self) -> Vec<PaneId> {
        match self {
            Self::Leaf { pane } => vec![*pane],
            Self::Split { first, second, .. } => {
                let mut v = first.leaves();
                v.extend(second.leaves());
                v
            }
        }
    }

    #[must_use]
    pub fn contains(&self, id: PaneId) -> bool {
        match self {
            Self::Leaf { pane } => *pane == id,
            Self::Split { first, second, .. } => first.contains(id) || second.contains(id),
        }
    }

    /// Split the leaf `target` toward `direction`, putting `new` on that side.
    /// Unchanged when `target` isn't here.
    #[must_use]
    pub fn splitting(&self, target: PaneId, direction: Direction, new: PaneId) -> Self {
        match self {
            Self::Leaf { pane } if *pane == target => {
                let existing = Box::new(self.clone());
                let fresh = Box::new(Self::Leaf { pane: new });
                let (first, second) = if direction.inserts_after() { (existing, fresh) } else { (fresh, existing) };
                Self::Split {
                    horizontal: direction.is_horizontal(),
                    first,
                    second,
                    fraction: 0.5,
                }
            }
            Self::Leaf { .. } => self.clone(),
            Self::Split {
                horizontal,
                first,
                second,
                fraction,
            } => Self::Split {
                horizontal: *horizontal,
                first: Box::new(first.splitting(target, direction, new)),
                second: Box::new(second.splitting(target, direction, new)),
                fraction: *fraction,
            },
        }
    }

    /// Remove a leaf, collapsing the split that held it. `None` when the tree
    /// was that single leaf.
    #[must_use]
    pub fn removing(&self, target: PaneId) -> Option<Self> {
        match self {
            Self::Leaf { pane } => (*pane != target).then(|| self.clone()),
            Self::Split {
                horizontal,
                first,
                second,
                fraction,
            } => match (first.removing(target), second.removing(target)) {
                (Some(a), Some(b)) => Some(Self::Split {
                    horizontal: *horizontal,
                    first: Box::new(a),
                    second: Box::new(b),
                    fraction: *fraction,
                }),
                (Some(a), None) => Some(a),
                (None, _) => Some((**second).clone()),
            },
        }
    }

    /// Every divider back to an even split.
    #[must_use]
    pub fn equalized(&self) -> Self {
        match self {
            Self::Leaf { .. } => self.clone(),
            Self::Split { horizontal, first, second, .. } => Self::Split {
                horizontal: *horizontal,
                first: Box::new(first.equalized()),
                second: Box::new(second.equalized()),
                fraction: 0.5,
            },
        }
    }

    /// The frame of every pane inside `bounds`. A vertical split puts `first`
    /// on top.
    #[must_use]
    pub fn frames(&self, bounds: Rect) -> BTreeMap<PaneId, Rect> {
        let mut out = BTreeMap::new();
        self.frames_into(bounds, &mut out);
        out
    }

    fn frames_into(&self, b: Rect, out: &mut BTreeMap<PaneId, Rect>) {
        match self {
            Self::Leaf { pane } => {
                out.insert(*pane, b);
            }
            Self::Split {
                horizontal,
                first,
                second,
                fraction,
            } => {
                let (r1, r2) = if *horizontal {
                    let w = b.w * fraction;
                    (Rect { w, ..b }, Rect { x: b.x + w, w: b.w - w, ..b })
                } else {
                    let h = b.h * fraction;
                    (Rect { h, ..b }, Rect { y: b.y + h, h: b.h - h, ..b })
                };
                first.frames_into(r1, out);
                second.frames_into(r2, out);
            }
        }
    }
}

/// Floating-point slack when comparing pane edges.
const EDGE_TOLERANCE: f64 = 0.5;

/// The pane lying `direction` from `from`.
///
/// Among panes actually that way, the nearest edge wins, ties broken by
/// centerline distance on the other axis — the rule tiling window managers
/// use — and an exact tie by the lower pane id. That last rule is normative (spec §5): iterating a hash map, as the
/// first Swift port did, makes a tie land differently from run to run.
#[must_use]
pub fn pane_in_direction(direction: Direction, from: PaneId, frames: &BTreeMap<PaneId, Rect>) -> Option<PaneId> {
    let origin = *frames.get(&from)?;
    let mut best: Option<(PaneId, f64, f64)> = None;
    for (&id, &r) in frames {
        if id == from {
            continue;
        }
        let (primary, secondary) = match direction {
            Direction::Left if r.mid_x() < origin.mid_x() => (origin.x - r.max_x(), (r.mid_y() - origin.mid_y()).abs()),
            Direction::Right if r.mid_x() > origin.mid_x() => (r.x - origin.max_x(), (r.mid_y() - origin.mid_y()).abs()),
            Direction::Up if r.mid_y() < origin.mid_y() => (origin.y - r.max_y(), (r.mid_x() - origin.mid_x()).abs()),
            Direction::Down if r.mid_y() > origin.mid_y() => (r.y - origin.max_y(), (r.mid_x() - origin.mid_x()).abs()),
            _ => continue,
        };
        // Only a pane beyond the origin's edge is "that way". A pane that
        // overlaps it on the primary axis (the full-height pane beside a
        // stack) has a centre that way but isn't: the first Swift port let it
        // win with a negative distance, so ↓ from the top of a stack jumped
        // sideways instead of down.
        if primary < -EDGE_TOLERANCE {
            continue;
        }
        if best.is_some_and(|(_, p, s)| (p, s) <= (primary, secondary)) {
            continue;
        }
        best = Some((id, primary, secondary));
    }
    best.map(|(id, _, _)| id)
}

/// One tab: a layout, which pane is focused, whether one is zoomed, and which
/// session each pane shows (a pane with no entry is empty).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Tab {
    pub root: Node,
    pub focused: PaneId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zoomed: Option<PaneId>,
    #[serde(default)]
    pub sessions: BTreeMap<PaneId, String>,
}

impl Tab {
    /// A tab of one pane, optionally showing `session`.
    #[must_use]
    pub fn new(pane: PaneId, session: Option<&str>) -> Self {
        let mut sessions = BTreeMap::new();
        if let Some(s) = session {
            sessions.insert(pane, s.to_string());
        }
        Self {
            root: Node::Leaf { pane },
            focused: pane,
            zoomed: None,
            sessions,
        }
    }

    #[must_use]
    pub fn panes(&self) -> Vec<PaneId> {
        self.root.leaves()
    }

    /// Split the focused pane; the new pane (id `new`) shows what the focused
    /// one showed and takes focus. Zoom ends.
    pub fn split(&mut self, direction: Direction, new: PaneId) {
        self.root = self.root.splitting(self.focused, direction, new);
        if let Some(s) = self.sessions.get(&self.focused).cloned() {
            self.sessions.insert(new, s);
        }
        self.focused = new;
        self.zoomed = None;
    }

    /// Close the focused pane; focus moves to the pane before it in layout
    /// order (or the next, for the first). False when it was the only pane —
    /// the caller applies the close scope (spec §5).
    pub fn close_focused(&mut self) -> bool {
        let order = self.panes();
        if order.len() < 2 {
            return false;
        }
        let gone = self.focused;
        let next = order
            .iter()
            .position(|p| *p == gone)
            .map_or(order[0], |i| order[if i == 0 { 1 } else { i - 1 }]);
        let Some(root) = self.root.removing(gone) else { return false };
        self.root = root;
        self.sessions.remove(&gone);
        self.focused = if self.root.contains(next) { next } else { self.root.leaves()[0] };
        if self.zoomed == Some(gone) {
            self.zoomed = None;
        }
        true
    }

    /// Move focus toward `direction` within `bounds`; no-op while zoomed.
    pub fn focus(&mut self, direction: Direction, bounds: Rect) {
        if self.zoomed.is_some() {
            return;
        }
        if let Some(next) = pane_in_direction(direction, self.focused, &self.root.frames(bounds)) {
            self.focused = next;
        }
    }

    /// Zoom the focused pane, or restore the layout. A single pane never zooms.
    pub fn toggle_zoom(&mut self) {
        self.zoomed = if self.panes().len() < 2 || self.zoomed.is_some() {
            None
        } else {
            Some(self.focused)
        };
    }

    /// Every divider even; zoom ends.
    pub fn equalize(&mut self) {
        self.root = self.root.equalized();
        self.zoomed = None;
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "unwrap is the idiom for test assertions")]
mod tests {
    use super::*;

    const B: Rect = Rect {
        x: 0.0,
        y: 0.0,
        w: 100.0,
        h: 100.0,
    };

    #[test]
    fn splits_place_the_new_pane_on_the_named_side() {
        let mut t = Tab::new(1, Some("fs-a"));
        t.split(Direction::Right, 2);
        assert_eq!(t.panes(), vec![1, 2]);
        assert_eq!(t.focused, 2);
        assert_eq!(t.sessions.get(&2).map(String::as_str), Some("fs-a"), "a split shows what it was split from");
        t.split(Direction::Up, 3);
        assert_eq!(t.panes(), vec![1, 3, 2], "up puts the new pane before the focused one");
        let f = t.root.frames(B);
        assert!(f[&3].y < f[&2].y, "3 is above 2");
    }

    #[test]
    fn closing_moves_focus_back_and_collapses_the_split() {
        let mut t = Tab::new(1, None);
        t.split(Direction::Right, 2);
        t.split(Direction::Down, 3);
        assert!(t.close_focused());
        assert_eq!(t.focused, 2);
        assert_eq!(t.panes(), vec![1, 2]);
        t.focused = 1;
        assert!(t.close_focused());
        assert_eq!(t.panes(), vec![2]);
        assert!(!t.close_focused(), "the last pane is the caller's call");
    }

    #[test]
    fn directional_focus_picks_the_nearest_pane_that_way() {
        let mut t = Tab::new(1, None);
        t.split(Direction::Right, 2);
        t.split(Direction::Down, 3);
        t.focus(Direction::Left, B);
        assert_eq!(t.focused, 1);
        t.focus(Direction::Right, B);
        assert_eq!(t.focused, 2, "a tie (2 and 3 are equally near) goes to the lower pane id");
        t.focus(Direction::Down, B);
        assert_eq!(t.focused, 3);
        t.focus(Direction::Down, B);
        assert_eq!(t.focused, 3, "nothing further down");
        t.focus(Direction::Up, B);
        assert_eq!(t.focused, 2, "up the stack, not sideways to the tall pane");
    }

    #[test]
    fn zoom_and_equalize() {
        let mut t = Tab::new(1, None);
        t.toggle_zoom();
        assert_eq!(t.zoomed, None, "one pane never zooms");
        t.split(Direction::Right, 2);
        t.toggle_zoom();
        assert_eq!(t.zoomed, Some(2));
        t.focus(Direction::Left, B);
        assert_eq!(t.focused, 2, "no moving focus while zoomed");
        t.toggle_zoom();
        assert_eq!(t.zoomed, None);
        if let Node::Split { fraction, .. } = &mut t.root {
            *fraction = 0.8;
        }
        t.equalize();
        assert!(matches!(t.root, Node::Split { fraction, .. } if (fraction - 0.5).abs() < f64::EPSILON));
    }
}

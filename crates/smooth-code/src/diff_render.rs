//! Differential TUI rendering — only re-render lines that changed.
//!
//! Instead of clearing and re-rendering the entire screen every frame, this
//! module tracks previously rendered lines and determines the minimal update
//! strategy. The synchronized output escape sequences (CSI 2026) make all
//! updates atomic and flicker-free.

/// Tracks rendered lines for differential updates.
pub struct RenderCache {
    /// Previously rendered lines (raw string content per line).
    prev_lines: Vec<String>,
    /// Terminal width at last render.
    prev_width: u16,
    /// Whether this is the first render (no cache yet).
    first_render: bool,
}

/// Render strategy that minimizes terminal output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenderStrategy {
    /// Full re-render (first frame, resize).
    Full,
    /// Partial re-render from a specific line.
    Partial {
        /// Index of the first line that differs.
        first_changed: usize,
    },
    /// Nothing changed, skip render.
    Skip,
}

impl RenderCache {
    /// Create a new empty render cache.
    pub fn new() -> Self {
        Self {
            prev_lines: Vec::new(),
            prev_width: 0,
            first_render: true,
        }
    }

    /// Compare new lines to cached lines. Returns the index of the first
    /// changed line, or `None` if nothing changed.
    pub fn first_changed_line(&self, new_lines: &[String]) -> Option<usize> {
        // If lengths differ, the first differing line is at most at the shorter length.
        let min_len = self.prev_lines.len().min(new_lines.len());

        for (i, new_line) in new_lines.iter().enumerate().take(min_len) {
            if self.prev_lines[i] != *new_line {
                return Some(i);
            }
        }

        // If lengths differ, the change starts at the end of the shorter slice.
        if self.prev_lines.len() != new_lines.len() {
            return Some(min_len);
        }

        None
    }

    /// Update the cache with new lines.
    pub fn update(&mut self, new_lines: Vec<String>, width: u16) {
        self.prev_lines = new_lines;
        self.prev_width = width;
        self.first_render = false;
    }

    /// Check if a full re-render is needed (width changed, first render).
    pub fn needs_full_render(&self, width: u16) -> bool {
        self.first_render || self.prev_width != width
    }

    /// Begin synchronized output (CSI 2026h).
    ///
    /// Tells the terminal to buffer all subsequent output until the
    /// matching end sequence, preventing partial-frame flicker.
    pub fn begin_sync() -> &'static str {
        "\x1b[?2026h"
    }

    /// End synchronized output (CSI 2026l).
    ///
    /// Tells the terminal to flush the buffered output atomically.
    pub fn end_sync() -> &'static str {
        "\x1b[?2026l"
    }

    /// Determine the optimal render strategy.
    pub fn strategy(&self, new_lines: &[String], width: u16) -> RenderStrategy {
        if self.needs_full_render(width) {
            return RenderStrategy::Full;
        }

        self.first_changed_line(new_lines)
            .map_or(RenderStrategy::Skip, |idx| RenderStrategy::Partial { first_changed: idx })
    }
}

impl Default for RenderCache {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_cache_needs_full_render() {
        let cache = RenderCache::new();
        assert!(cache.needs_full_render(80));
        assert!(cache.first_render);
        assert!(cache.prev_lines.is_empty());
    }

    #[test]
    fn first_changed_line_detects_change_at_position() {
        let mut cache = RenderCache::new();
        cache.update(vec!["line 0".into(), "line 1".into(), "line 2".into(), "line 3".into()], 80);

        let new_lines: Vec<String> = vec!["line 0".into(), "line 1".into(), "CHANGED".into(), "line 3".into()];
        assert_eq!(cache.first_changed_line(&new_lines), Some(2));
    }

    #[test]
    fn first_changed_line_returns_none_when_identical() {
        let mut cache = RenderCache::new();
        let lines: Vec<String> = vec!["alpha".into(), "beta".into(), "gamma".into()];
        cache.update(lines.clone(), 80);

        assert_eq!(cache.first_changed_line(&lines), None);
    }

    #[test]
    fn needs_full_render_on_width_change() {
        let mut cache = RenderCache::new();
        cache.update(vec!["hello".into()], 80);

        assert!(!cache.needs_full_render(80));
        assert!(cache.needs_full_render(120));
    }

    #[test]
    fn strategy_returns_full_on_first_render() {
        let cache = RenderCache::new();
        let lines: Vec<String> = vec!["test".into()];
        assert_eq!(cache.strategy(&lines, 80), RenderStrategy::Full);
    }

    #[test]
    fn strategy_returns_skip_when_nothing_changed() {
        let mut cache = RenderCache::new();
        let lines: Vec<String> = vec!["stable".into(), "content".into()];
        cache.update(lines.clone(), 80);

        assert_eq!(cache.strategy(&lines, 80), RenderStrategy::Skip);
    }
}

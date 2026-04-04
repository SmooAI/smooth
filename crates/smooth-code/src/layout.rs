//! Layout computation for the 3-panel TUI.

use ratatui::layout::{Constraint, Direction, Layout, Rect};

/// Regions of the TUI layout.
pub struct LayoutRegions {
    /// Optional sidebar (file tree, context, etc.).
    pub sidebar: Option<Rect>,
    /// Main chat/message area.
    pub chat: Rect,
    /// Text input area.
    pub input: Rect,
    /// Bottom status bar.
    pub status: Rect,
}

/// Sidebar width as a percentage of total width.
const SIDEBAR_PERCENT: u16 = 20;

/// Minimum sidebar width in columns.
const SIDEBAR_MIN_WIDTH: u16 = 15;

/// Input area height in rows.
const INPUT_HEIGHT: u16 = 3;

/// Status bar height in rows.
const STATUS_HEIGHT: u16 = 1;

/// Compute the layout regions for the given terminal area.
pub fn compute_layout(area: Rect, sidebar_visible: bool) -> LayoutRegions {
    // Vertical split: chat area | input | status
    let vertical_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),                // chat (takes remaining space)
            Constraint::Length(INPUT_HEIGHT),  // input
            Constraint::Length(STATUS_HEIGHT), // status
        ])
        .split(area);

    let chat_area = vertical_chunks[0];
    let input = vertical_chunks[1];
    let status = vertical_chunks[2];

    if sidebar_visible && area.width >= SIDEBAR_MIN_WIDTH * 2 {
        // Horizontal split: sidebar | chat
        let sidebar_width = (area.width * SIDEBAR_PERCENT / 100).max(SIDEBAR_MIN_WIDTH);
        let horizontal_chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(sidebar_width), Constraint::Min(1)])
            .split(chat_area);

        LayoutRegions {
            sidebar: Some(horizontal_chunks[0]),
            chat: horizontal_chunks[1],
            input,
            status,
        }
    } else {
        LayoutRegions {
            sidebar: None,
            chat: chat_area,
            input,
            status,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_layout_without_sidebar() {
        let area = Rect::new(0, 0, 80, 24);
        let regions = compute_layout(area, false);

        assert!(regions.sidebar.is_none());
        assert_eq!(regions.chat.width, 80);
        assert_eq!(regions.input.height, INPUT_HEIGHT);
        assert_eq!(regions.status.height, STATUS_HEIGHT);
        // Chat should take remaining vertical space
        assert_eq!(regions.chat.height, 24 - INPUT_HEIGHT - STATUS_HEIGHT);
    }

    #[test]
    fn test_layout_with_sidebar() {
        let area = Rect::new(0, 0, 100, 30);
        let regions = compute_layout(area, true);

        assert!(regions.sidebar.is_some());
        let sidebar = regions.sidebar.expect("sidebar should be present");
        // 20% of 100 = 20, which is >= SIDEBAR_MIN_WIDTH
        assert_eq!(sidebar.width, 20);
        assert_eq!(regions.chat.width, 80);
        assert_eq!(regions.input.height, INPUT_HEIGHT);
        assert_eq!(regions.status.height, STATUS_HEIGHT);
    }

    #[test]
    fn test_layout_small_terminal() {
        // Terminal too narrow for sidebar — should not show it even if requested
        let area = Rect::new(0, 0, 20, 10);
        let regions = compute_layout(area, true);

        // 20 < SIDEBAR_MIN_WIDTH * 2 (30), so sidebar should be hidden
        assert!(regions.sidebar.is_none());
        assert_eq!(regions.chat.width, 20);
    }
}

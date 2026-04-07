//! Project selector overlay — modal list of projects fetched from the gateway.
//!
//! Renders as a centred popup over the conversation area with keyboard
//! navigation for selecting a project. Selecting a project updates the active
//! project context.

use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Color, Style},
    text::Line,
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph},
};

use crate::state::ProjectInfo;

/// State for the project selector modal overlay.
///
/// Navigation wraps around at both ends — selecting past the last item
/// returns to the first, and selecting before the first goes to the last.
#[derive(Debug, Default)]
pub struct ProjectSelectorState {
    pub projects: Vec<ProjectInfo>,
    /// Index of the currently highlighted project.
    pub selected_index: usize,
    /// True while the project list is being fetched from the gateway.
    pub is_loading: bool,
    /// Error message to display instead of the list when a fetch fails.
    pub error: Option<String>,
    /// Whether the overlay is currently shown.
    pub is_visible: bool,
}

impl ProjectSelectorState {
    /// Create a new, hidden `ProjectSelectorState`.
    pub fn new() -> Self {
        Self::default()
    }

    /// Move the selection one item down, wrapping to the first item after the last.
    pub fn select_next(&mut self) {
        if self.projects.is_empty() {
            return;
        }
        self.selected_index = (self.selected_index + 1) % self.projects.len();
    }

    /// Move the selection one item up, wrapping to the last item before the first.
    pub fn select_previous(&mut self) {
        if self.projects.is_empty() {
            return;
        }
        self.selected_index = if self.selected_index == 0 {
            self.projects.len() - 1
        } else {
            self.selected_index - 1
        };
    }

    /// Return the currently selected `ProjectInfo`, or `None` if the list is empty.
    pub fn confirm(&self) -> Option<ProjectInfo> {
        self.projects.get(self.selected_index).cloned()
    }

    /// Replace the project list and reset the selection to the first item.
    pub fn set_projects(&mut self, projects: Vec<ProjectInfo>) {
        self.projects = projects;
        self.selected_index = 0;
        self.is_loading = false;
        self.error = None;
    }

    /// Set an error message and clear the loading flag.
    pub fn set_error(&mut self, message: String) {
        self.error = Some(message);
        self.is_loading = false;
    }

    /// Show the overlay, clearing any previous error state.
    pub fn show(&mut self) {
        self.is_visible = true;
        self.error = None;
    }

    /// Hide the overlay.
    pub fn hide(&mut self) {
        self.is_visible = false;
    }

    /// Return `true` if the overlay is currently visible.
    pub fn is_visible(&self) -> bool {
        self.is_visible
    }
}

// ---- Rendering -----------------------------------------------------------

/// Render the project selector overlay centred within `area`.
///
/// If `state.is_visible` is false the function returns without rendering
/// anything, so callers can always invoke it unconditionally.
///
/// The popup clears the background beneath it (via `Clear`) before drawing,
/// so underlying widgets do not bleed through.
pub fn render_project_selector(frame: &mut Frame, area: Rect, state: &ProjectSelectorState) {
    if !state.is_visible {
        return;
    }

    let popup = popup_area(area);

    // Clear the background before drawing the popup.
    frame.render_widget(Clear, popup);

    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Select Project ");

    let inner = block.inner(popup);

    // Split inner area into content region and footer hint row.
    let chunks = Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).split(inner);
    let content_area = chunks[0];
    let footer_area = chunks[1];

    // Render the bordered block shell first.
    frame.render_widget(block, popup);

    // ---- Content ----
    if state.is_loading {
        let loading = Paragraph::new("Loading projects...")
            .alignment(Alignment::Center)
            .style(Style::default().fg(Color::DarkGray));
        frame.render_widget(loading, content_area);
    } else if let Some(err) = &state.error {
        let error_text = Paragraph::new(err.as_str())
            .style(Style::default().fg(Color::Red))
            .alignment(Alignment::Left);
        frame.render_widget(error_text, content_area);
    } else {
        let items: Vec<ListItem> = state
            .projects
            .iter()
            .map(|p| ListItem::new(p.name.as_str()))
            .collect();

        let list =
            List::new(items).highlight_style(Style::default().fg(Color::White).bg(Color::Blue));

        // Feed our tracked index into ratatui's ListState for rendering.
        let mut list_state = ListState::default();
        if !state.projects.is_empty() {
            list_state.select(Some(state.selected_index));
        }

        frame.render_stateful_widget(list, content_area, &mut list_state);
    }

    // ---- Footer hint ----
    let hint = Paragraph::new(Line::from("↑↓ Navigate  Enter Select  Esc Cancel"))
        .style(Style::default().fg(Color::DarkGray))
        .alignment(Alignment::Center);
    frame.render_widget(hint, footer_area);
}

/// Compute the centred popup `Rect` for the given terminal `area`.
///
/// Width: 60 % of area width, capped at 40 columns.
/// Height: 50 % of area height, capped at 15 rows.
/// Minimum dimensions ensure the popup is always at least marginally usable.
fn popup_area(area: Rect) -> Rect {
    let w = (area.width * 60 / 100).min(40).max(10);
    let h = (area.height * 50 / 100).min(15).max(5);
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    Rect::new(x, y, w, h)
}

// ---- Tests ---------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn make_project(name: &str) -> ProjectInfo {
        ProjectInfo {
            id: Uuid::new_v4(),
            name: name.to_string(),
        }
    }

    fn three_projects() -> Vec<ProjectInfo> {
        vec![
            make_project("Alpha"),
            make_project("Beta"),
            make_project("Gamma"),
        ]
    }

    // ---- select_next ----------------------------------------------------

    #[test]
    fn select_next_advances_index() {
        let mut state = ProjectSelectorState::new();
        state.set_projects(three_projects());
        state.select_next();
        assert_eq!(state.selected_index, 1);
    }

    #[test]
    fn select_next_wraps_from_last_to_first() {
        let mut state = ProjectSelectorState::new();
        state.set_projects(three_projects());
        state.selected_index = 2; // last item
        state.select_next();
        assert_eq!(state.selected_index, 0, "should wrap to first");
    }

    #[test]
    fn select_next_on_empty_list_is_noop() {
        let mut state = ProjectSelectorState::new();
        state.select_next();
        assert_eq!(state.selected_index, 0);
    }

    // ---- select_previous ------------------------------------------------

    #[test]
    fn select_previous_decrements_index() {
        let mut state = ProjectSelectorState::new();
        state.set_projects(three_projects());
        state.selected_index = 2;
        state.select_previous();
        assert_eq!(state.selected_index, 1);
    }

    #[test]
    fn select_previous_wraps_from_first_to_last() {
        let mut state = ProjectSelectorState::new();
        state.set_projects(three_projects());
        state.selected_index = 0; // first item
        state.select_previous();
        assert_eq!(state.selected_index, 2, "should wrap to last");
    }

    #[test]
    fn select_previous_on_empty_list_is_noop() {
        let mut state = ProjectSelectorState::new();
        state.select_previous();
        assert_eq!(state.selected_index, 0);
    }

    // ---- confirm --------------------------------------------------------

    #[test]
    fn confirm_returns_selected_project() {
        let mut state = ProjectSelectorState::new();
        state.set_projects(three_projects());
        state.selected_index = 1;
        let confirmed = state.confirm().unwrap();
        assert_eq!(confirmed.name, "Beta");
    }

    #[test]
    fn confirm_on_empty_list_returns_none() {
        let state = ProjectSelectorState::new();
        assert!(state.confirm().is_none());
    }

    #[test]
    fn confirm_does_not_mutate_state() {
        let mut state = ProjectSelectorState::new();
        state.set_projects(three_projects());
        let _ = state.confirm();
        assert_eq!(state.selected_index, 0, "confirm must not change selection");
    }

    // ---- set_projects ---------------------------------------------------

    #[test]
    fn set_projects_replaces_list_and_resets_selection() {
        let mut state = ProjectSelectorState::new();
        state.set_projects(three_projects());
        state.selected_index = 2;
        state.set_projects(vec![make_project("New")]);
        assert_eq!(state.projects.len(), 1);
        assert_eq!(state.selected_index, 0, "selection should reset to 0");
    }

    #[test]
    fn set_projects_clears_loading_and_error() {
        let mut state = ProjectSelectorState::new();
        state.is_loading = true;
        state.error = Some("previous error".to_string());
        state.set_projects(three_projects());
        assert!(!state.is_loading);
        assert!(state.error.is_none());
    }

    // ---- set_error ------------------------------------------------------

    #[test]
    fn set_error_stores_message_and_clears_loading() {
        let mut state = ProjectSelectorState::new();
        state.is_loading = true;
        state.set_error("fetch failed".to_string());
        assert_eq!(state.error.as_deref(), Some("fetch failed"));
        assert!(!state.is_loading);
    }

    // ---- show / hide / is_visible ---------------------------------------

    #[test]
    fn show_makes_overlay_visible() {
        let mut state = ProjectSelectorState::new();
        state.show();
        assert!(state.is_visible());
    }

    #[test]
    fn hide_makes_overlay_invisible() {
        let mut state = ProjectSelectorState::new();
        state.show();
        state.hide();
        assert!(!state.is_visible());
    }

    #[test]
    fn show_clears_previous_error() {
        let mut state = ProjectSelectorState::new();
        state.set_error("old error".to_string());
        state.show();
        assert!(state.error.is_none());
    }

    // ---- popup_area -----------------------------------------------------

    #[test]
    fn popup_area_is_centred_within_given_rect() {
        let terminal_area = Rect::new(0, 0, 100, 40);
        let popup = popup_area(terminal_area);
        // Should be centred horizontally and vertically.
        assert!(popup.x > 0, "popup should not start at left edge");
        assert!(popup.y > 0, "popup should not start at top edge");
        assert!(popup.right() <= 100, "popup should not exceed right edge");
        assert!(popup.bottom() <= 40, "popup should not exceed bottom edge");
    }

    #[test]
    fn popup_area_respects_max_dimensions() {
        // Even with a very large terminal, popup should cap at 40x15.
        let terminal_area = Rect::new(0, 0, 200, 100);
        let popup = popup_area(terminal_area);
        assert!(popup.width <= 40, "popup width should cap at 40");
        assert!(popup.height <= 15, "popup height should cap at 15");
    }

    #[test]
    fn popup_area_enforces_minimum_dimensions() {
        // Very small terminal: popup should be at least 10x5.
        let terminal_area = Rect::new(0, 0, 12, 6);
        let popup = popup_area(terminal_area);
        assert!(popup.width >= 10, "popup width must be at least 10");
        assert!(popup.height >= 5, "popup height must be at least 5");
    }

    // ---- render tests ---------------------------------------------------

    #[test]
    fn render_does_nothing_when_not_visible() {
        use ratatui::{Terminal, backend::TestBackend};
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let state = ProjectSelectorState::new(); // is_visible = false
        terminal
            .draw(|frame| render_project_selector(frame, frame.area(), &state))
            .unwrap();
        // Buffer should be entirely blank (no popup was rendered).
        let content: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        let non_space: String = content.chars().filter(|c| !c.is_whitespace()).collect();
        assert!(
            non_space.is_empty(),
            "hidden overlay must not render anything; got: {non_space:?}"
        );
    }

    #[test]
    fn render_shows_loading_state() {
        use ratatui::{Terminal, backend::TestBackend};
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = ProjectSelectorState::new();
        state.show();
        state.is_loading = true;
        terminal
            .draw(|frame| render_project_selector(frame, frame.area(), &state))
            .unwrap();
        let content: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(
            content.contains("Loading"),
            "loading state must show loading message; got: {content:?}"
        );
    }

    #[test]
    fn render_shows_error_state() {
        use ratatui::{Terminal, backend::TestBackend};
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = ProjectSelectorState::new();
        state.show();
        state.set_error("gateway unreachable".to_string());
        terminal
            .draw(|frame| render_project_selector(frame, frame.area(), &state))
            .unwrap();
        let content: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(
            content.contains("gateway unreachable"),
            "error state must display error message; got: {content:?}"
        );
    }

    #[test]
    fn render_shows_project_list() {
        use ratatui::{Terminal, backend::TestBackend};
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = ProjectSelectorState::new();
        state.set_projects(three_projects());
        state.show();
        terminal
            .draw(|frame| render_project_selector(frame, frame.area(), &state))
            .unwrap();
        let content: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(
            content.contains("Alpha"),
            "project list must show 'Alpha'; got: {content:?}"
        );
        assert!(
            content.contains("Beta"),
            "project list must show 'Beta'; got: {content:?}"
        );
        assert!(
            content.contains("Gamma"),
            "project list must show 'Gamma'; got: {content:?}"
        );
    }

    #[test]
    fn render_shows_footer_hint() {
        use ratatui::{Terminal, backend::TestBackend};
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = ProjectSelectorState::new();
        state.set_projects(three_projects());
        state.show();
        terminal
            .draw(|frame| render_project_selector(frame, frame.area(), &state))
            .unwrap();
        let content: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(
            content.contains("Navigate"),
            "footer hint must mention navigation; got: {content:?}"
        );
    }

    #[test]
    fn render_shows_select_project_title() {
        use ratatui::{Terminal, backend::TestBackend};
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = ProjectSelectorState::new();
        state.set_projects(three_projects());
        state.show();
        terminal
            .draw(|frame| render_project_selector(frame, frame.area(), &state))
            .unwrap();
        let content: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(
            content.contains("Select Project"),
            "popup must show 'Select Project' title; got: {content:?}"
        );
    }
}

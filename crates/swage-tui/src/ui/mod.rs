//! Layout calculation and main render function.
//!
//! Divides the terminal into three fixed regions (status bar, conversation area,
//! input area) with an optional context inspector panel that splits the
//! conversation area horizontally when toggled on.

pub mod approval_panel;
pub mod conversation;
pub mod input;
pub mod inspector;
pub mod status_bar;
pub mod topic_selector;

use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Color, Style},
    widgets::Paragraph,
};

use crate::gateway::ConnectionState;
use crate::state::ConversationState;
use crate::ui::input::InputState;
use crate::ui::topic_selector::TopicSelectorState;

/// Minimum terminal width required for the full TUI layout.
const MIN_WIDTH: u16 = 80;
/// Minimum terminal height required for the full TUI layout.
const MIN_HEIGHT: u16 = 24;

/// Computed layout regions for a single render pass.
///
/// Extracted from the render function to make layout logic independently
/// testable without a live terminal backend.
#[derive(Debug, Clone, Copy)]
pub struct LayoutRegions {
    /// Top row: connection indicator, topic name, model, tokens.
    pub status_area: Rect,
    /// Middle region: message history and streaming output.
    pub conversation_area: Rect,
    /// Right panel alongside the conversation area (Some when inspector is open).
    pub inspector_area: Option<Rect>,
    /// Bottom region: text input.
    pub input_area: Rect,
}

/// All state required for a single render pass.
///
/// Bundles the state pointers and view-flag values the render function needs,
/// keeping the signature stable as new flags are added.
pub struct RenderParams<'a> {
    pub state: &'a ConversationState,
    pub input_state: &'a InputState,
    pub selector_state: &'a TopicSelectorState,
    pub connection_state: ConnectionState,
    /// True when the context inspector panel should be shown on the right.
    pub is_inspector_open: bool,
}

// ---- Public render entry point ------------------------------------------

/// Render one complete frame using `params`.
///
/// If the terminal is narrower than 80 columns or shorter than 24 rows a
/// yellow resize prompt is shown instead of the full layout. All widget
/// calls are unconditional; each widget guards its own visibility internally.
pub fn render(frame: &mut Frame, params: &RenderParams<'_>) {
    let area = frame.area();
    let input_height = compute_input_height(params.input_state);

    let regions = match calculate_layout(area, input_height, params.is_inspector_open) {
        Some(r) => r,
        None => {
            // Terminal too small — tell the user rather than garbling the UI.
            let prompt = Paragraph::new("Terminal too small — resize to at least 80×24")
                .style(Style::default().fg(Color::Yellow))
                .alignment(Alignment::Center);
            frame.render_widget(prompt, area);
            return;
        }
    };

    let state = params.state;

    // Status bar (always visible).
    status_bar::render_status_bar(
        frame,
        regions.status_area,
        params.connection_state,
        &state.active_topic.name,
        state.model_name.as_deref(),
        state.input_tokens,
        state.output_tokens,
    );

    // Conversation history.
    conversation::render_conversation(
        frame,
        regions.conversation_area,
        state.messages(),
        state.streaming_buffer(),
        state.is_streaming(),
        state.active_tools(),
        state.scroll_offset(),
    );

    // Context inspector (optional right panel).
    if let Some(inspector_rect) = regions.inspector_area {
        inspector::render_inspector(frame, inspector_rect, state.context_report.as_ref());
    }

    // Input area — disabled when not connected.
    let is_disabled = params.connection_state != ConnectionState::Connected;
    input::render_input(frame, regions.input_area, params.input_state, is_disabled);

    // Topic selector overlay — renders last so it sits on top of everything.
    // render_topic_selector is a no-op when selector_state.is_visible == false.
    topic_selector::render_topic_selector(frame, area, params.selector_state);
}

// ---- Layout helpers (pub(crate) for unit testing) -----------------------

/// Compute the vertical height needed for the input area.
///
/// Single-line: 3 rows (border-top + 1 content + border-bottom).
/// Multi-line: border rows + content rows, capped so the input never
/// takes more than 7 rows and leaves room for the conversation above.
pub(crate) fn compute_input_height(input_state: &InputState) -> u16 {
    if input_state.is_multi_line {
        // Count logical lines (split on explicit newlines), then add borders.
        let content_lines = input_state.text.split('\n').count().max(1);
        ((content_lines + 2) as u16).min(7)
    } else {
        3
    }
}

/// Calculate layout regions for the given `area` and parameters.
///
/// Returns `None` when the terminal is below the minimum size. The layout
/// is deterministic for a given (area, input_height, is_inspector_open)
/// triplet, making it easy to test independently.
pub(crate) fn calculate_layout(
    area: Rect,
    input_height: u16,
    is_inspector_open: bool,
) -> Option<LayoutRegions> {
    if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
        return None;
    }

    // Main vertical split: status (top), middle, input (bottom).
    let v_chunks = Layout::vertical([
        Constraint::Length(1),            // status bar
        Constraint::Fill(1),              // conversation + optional inspector
        Constraint::Length(input_height), // input area
    ])
    .split(area);

    let status_area = v_chunks[0];
    let middle_area = v_chunks[1];
    let input_area = v_chunks[2];

    // Optional horizontal split for the inspector panel.
    let (conversation_area, inspector_area) = if is_inspector_open {
        let h_chunks = Layout::horizontal([
            Constraint::Percentage(70), // conversation
            Constraint::Percentage(30), // inspector
        ])
        .split(middle_area);
        (h_chunks[0], Some(h_chunks[1]))
    } else {
        (middle_area, None)
    };

    Some(LayoutRegions {
        status_area,
        conversation_area,
        inspector_area,
        input_area,
    })
}

// ---- Tests ---------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gateway::ConnectionState;
    use crate::state::{ConversationState, TopicInfo};
    use crate::ui::input::InputState;
    use crate::ui::topic_selector::TopicSelectorState;
    use uuid::Uuid;

    fn default_topic() -> TopicInfo {
        TopicInfo {
            id: Uuid::new_v4(),
            name: "TestTopic".to_string(),
        }
    }

    // ---- compute_input_height -------------------------------------------

    #[test]
    fn input_height_single_line_is_three() {
        let state = InputState::new();
        assert_eq!(compute_input_height(&state), 3);
    }

    #[test]
    fn input_height_multi_line_empty_text_is_three() {
        let mut state = InputState::new();
        state.is_multi_line = true;
        // Empty text = 1 content line + 2 borders = 3.
        assert_eq!(compute_input_height(&state), 3);
    }

    #[test]
    fn input_height_multi_line_grows_with_newlines() {
        let mut state = InputState::new();
        state.is_multi_line = true;
        state.text = "line one\nline two\nline three".to_string();
        // 3 content lines + 2 borders = 5.
        assert_eq!(compute_input_height(&state), 5);
    }

    #[test]
    fn input_height_multi_line_caps_at_seven() {
        let mut state = InputState::new();
        state.is_multi_line = true;
        // 10 lines → would be 12, but caps at 7.
        state.text = "a\nb\nc\nd\ne\nf\ng\nh\ni\nj".to_string();
        assert_eq!(compute_input_height(&state), 7);
    }

    // ---- calculate_layout -----------------------------------------------

    #[test]
    fn calculate_layout_returns_none_below_minimum_width() {
        let small = Rect::new(0, 0, 79, 24); // 1 col too narrow
        assert!(calculate_layout(small, 3, false).is_none());
    }

    #[test]
    fn calculate_layout_returns_none_below_minimum_height() {
        let small = Rect::new(0, 0, 80, 23); // 1 row too short
        assert!(calculate_layout(small, 3, false).is_none());
    }

    #[test]
    fn calculate_layout_returns_some_at_minimum_size() {
        let area = Rect::new(0, 0, 80, 24);
        assert!(calculate_layout(area, 3, false).is_some());
    }

    #[test]
    fn calculate_layout_status_area_is_one_row() {
        let area = Rect::new(0, 0, 80, 24);
        let regions = calculate_layout(area, 3, false).unwrap();
        assert_eq!(regions.status_area.height, 1);
        assert_eq!(regions.status_area.y, 0);
    }

    #[test]
    fn calculate_layout_input_area_height_matches_parameter() {
        let area = Rect::new(0, 0, 80, 24);
        let regions = calculate_layout(area, 5, false).unwrap();
        assert_eq!(regions.input_area.height, 5);
    }

    #[test]
    fn calculate_layout_areas_fill_the_terminal_height() {
        let area = Rect::new(0, 0, 80, 24);
        let input_height = 3u16;
        let regions = calculate_layout(area, input_height, false).unwrap();
        let total = regions.status_area.height
            + regions.conversation_area.height
            + regions.input_area.height;
        assert_eq!(
            total, area.height,
            "status + conversation + input should fill terminal height"
        );
    }

    #[test]
    fn calculate_layout_no_inspector_conversation_takes_full_width() {
        let area = Rect::new(0, 0, 80, 24);
        let regions = calculate_layout(area, 3, false).unwrap();
        assert!(regions.inspector_area.is_none());
        assert_eq!(regions.conversation_area.width, area.width);
    }

    #[test]
    fn calculate_layout_inspector_splits_middle_horizontally() {
        let area = Rect::new(0, 0, 100, 30);
        let regions = calculate_layout(area, 3, true).unwrap();
        let inspector = regions.inspector_area.expect("inspector should be present");
        assert!(regions.conversation_area.width > 0);
        assert!(inspector.width > 0);
        // Conversation + inspector should span the full width.
        assert_eq!(
            regions.conversation_area.width + inspector.width,
            area.width,
            "conversation + inspector widths should equal terminal width"
        );
    }

    #[test]
    fn calculate_layout_inspector_conversation_is_wider_than_inspector() {
        let area = Rect::new(0, 0, 100, 30);
        let regions = calculate_layout(area, 3, true).unwrap();
        let inspector = regions.inspector_area.unwrap();
        assert!(
            regions.conversation_area.width > inspector.width,
            "conversation (70%) should be wider than inspector (30%)"
        );
    }

    #[test]
    fn calculate_layout_areas_have_same_x_origin_as_terminal() {
        let area = Rect::new(5, 10, 80, 24); // non-zero origin
        let regions = calculate_layout(area, 3, false).unwrap();
        assert_eq!(regions.status_area.x, 5);
        assert_eq!(regions.conversation_area.x, 5);
        assert_eq!(regions.input_area.x, 5);
    }

    // ---- render smoke tests ---------------------------------------------

    #[test]
    fn render_does_not_panic_with_standard_terminal() {
        use ratatui::{Terminal, backend::TestBackend};
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let conv_state = ConversationState::new(default_topic());
        let input_state = InputState::new();
        let selector_state = TopicSelectorState::new();
        let params = RenderParams {
            state: &conv_state,
            input_state: &input_state,
            selector_state: &selector_state,
            connection_state: ConnectionState::Connected,
            is_inspector_open: false,
        };
        terminal.draw(|frame| render(frame, &params)).unwrap();
    }

    #[test]
    fn render_does_not_panic_with_inspector_open() {
        use ratatui::{Terminal, backend::TestBackend};
        let backend = TestBackend::new(120, 40);
        let mut terminal = Terminal::new(backend).unwrap();
        let conv_state = ConversationState::new(default_topic());
        let input_state = InputState::new();
        let selector_state = TopicSelectorState::new();
        let params = RenderParams {
            state: &conv_state,
            input_state: &input_state,
            selector_state: &selector_state,
            connection_state: ConnectionState::Connected,
            is_inspector_open: true,
        };
        terminal.draw(|frame| render(frame, &params)).unwrap();
    }

    #[test]
    fn render_shows_resize_prompt_on_small_terminal() {
        use ratatui::{Terminal, backend::TestBackend};
        let backend = TestBackend::new(60, 20); // below 80×24
        let mut terminal = Terminal::new(backend).unwrap();
        let conv_state = ConversationState::new(default_topic());
        let input_state = InputState::new();
        let selector_state = TopicSelectorState::new();
        let params = RenderParams {
            state: &conv_state,
            input_state: &input_state,
            selector_state: &selector_state,
            connection_state: ConnectionState::Disconnected,
            is_inspector_open: false,
        };
        terminal.draw(|frame| render(frame, &params)).unwrap();
        let content: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(
            content.contains("too small"),
            "small terminal should show resize prompt; got: {content:?}"
        );
    }

    #[test]
    fn render_does_not_panic_with_topic_selector_open() {
        use ratatui::{Terminal, backend::TestBackend};
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let conv_state = ConversationState::new(default_topic());
        let input_state = InputState::new();
        let mut selector_state = TopicSelectorState::new();
        selector_state.show();
        let params = RenderParams {
            state: &conv_state,
            input_state: &input_state,
            selector_state: &selector_state,
            connection_state: ConnectionState::Connected,
            is_inspector_open: false,
        };
        terminal.draw(|frame| render(frame, &params)).unwrap();
    }

    #[test]
    fn render_does_not_panic_when_disconnected() {
        use ratatui::{Terminal, backend::TestBackend};
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let conv_state = ConversationState::new(default_topic());
        let mut input_state = InputState::new();
        input_state.text = "draft message".to_string();
        input_state.cursor_position = 5;
        let selector_state = TopicSelectorState::new();
        let params = RenderParams {
            state: &conv_state,
            input_state: &input_state,
            selector_state: &selector_state,
            connection_state: ConnectionState::Disconnected,
            is_inspector_open: false,
        };
        terminal.draw(|frame| render(frame, &params)).unwrap();
    }

    #[test]
    fn render_does_not_panic_with_multiline_input() {
        use ratatui::{Terminal, backend::TestBackend};
        let backend = TestBackend::new(80, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        let conv_state = ConversationState::new(default_topic());
        let mut input_state = InputState::new();
        input_state.is_multi_line = true;
        input_state.text = "line 1\nline 2\nline 3".to_string();
        input_state.cursor_position = 14;
        let selector_state = TopicSelectorState::new();
        let params = RenderParams {
            state: &conv_state,
            input_state: &input_state,
            selector_state: &selector_state,
            connection_state: ConnectionState::Connected,
            is_inspector_open: false,
        };
        terminal.draw(|frame| render(frame, &params)).unwrap();
    }
}

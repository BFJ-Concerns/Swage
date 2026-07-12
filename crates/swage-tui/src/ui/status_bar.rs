//! Status bar widget — persistent top row showing connection state, space
//! name, model name, and token counts.
//!
//! Renders as a single-line ratatui widget with colour-coded connection state
//! indicator and graceful degradation when observability data is unavailable.

use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Style},
    text::{Line, Span},
    widgets::Paragraph,
};

use crate::gateway::ConnectionState;

/// Render the status bar into `area` (expected to be exactly one row tall).
///
/// Left section: colour-coded connection indicator ("●") followed by the
/// space name. Right section: model name and token counts, right-aligned
/// by padding between the two sections. The right section degrades
/// gracefully when `area.width` is too small to fit it.
pub fn render_status_bar(
    frame: &mut Frame,
    area: Rect,
    connection_state: ConnectionState,
    space_name: &str,
    model_name: Option<&str>,
    input_tokens: Option<u32>,
    output_tokens: Option<u32>,
) {
    let line = build_status_line(
        connection_state,
        space_name,
        model_name,
        input_tokens,
        output_tokens,
        area.width as usize,
    );
    frame.render_widget(Paragraph::new(line), area);
}

/// Build the status bar `Line` for the given terminal `width`.
///
/// Separated from the render function to allow direct testing of span
/// content and colours without a terminal backend.
pub(crate) fn build_status_line(
    connection_state: ConnectionState,
    space_name: &str,
    model_name: Option<&str>,
    input_tokens: Option<u32>,
    output_tokens: Option<u32>,
    width: usize,
) -> Line<'static> {
    // Colour-code the connection indicator dot.
    let indicator_style = match connection_state {
        ConnectionState::Connected => Style::default().fg(Color::Green),
        ConnectionState::Disconnected => Style::default().fg(Color::Red),
        ConnectionState::Reconnecting => Style::default().fg(Color::Yellow),
    };

    // Left section is always shown: " ● space_name ".
    // Display width: 1 (leading space) + 1 (●) + 1 (space) + name + 1 (space).
    let left_suffix = format!(" {space_name} ");
    let left_width = 2 + left_suffix.chars().count(); // " " + "●" + " space_name "

    // Build right-side candidates, most detailed first.
    // Graceful truncation: drop token counts, then model, always keep left.
    let model_str = model_name.unwrap_or("—");
    let tokens_str = match (input_tokens, output_tokens) {
        (Some(i), Some(o)) => format!("in: {i} / out: {o}"),
        _ => "—".to_string(),
    };

    let full_right = format!("{model_str} │ {tokens_str} ");
    let model_right = format!("{model_str} ");

    let right: String = if left_width + full_right.chars().count() <= width {
        full_right
    } else if left_width + model_right.chars().count() <= width {
        model_right
    } else {
        // Terminal too narrow for any right-side content.
        String::new()
    };

    // Pad the middle so the right section is flush against the right edge.
    let padding = width.saturating_sub(left_width + right.chars().count());

    let mut spans = vec![
        Span::raw(" "),
        Span::styled("●", indicator_style),
        Span::raw(left_suffix),
        Span::raw(" ".repeat(padding)),
    ];

    if !right.is_empty() {
        spans.push(Span::raw(right));
    }

    Line::from(spans)
}

// ---- Tests ---------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gateway::ConnectionState;

    // ---- Helper: extract plain text from a Line -------------------------

    fn line_text(line: &Line<'_>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    // ---- Indicator presence ---------------------------------------------

    #[test]
    fn connected_indicator_appears_in_line() {
        let line = build_status_line(ConnectionState::Connected, "alpha", None, None, None, 80);
        assert!(line_text(&line).contains("●"));
    }

    #[test]
    fn disconnected_indicator_appears_in_line() {
        let line = build_status_line(ConnectionState::Disconnected, "beta", None, None, None, 80);
        assert!(line_text(&line).contains("●"));
    }

    #[test]
    fn reconnecting_indicator_appears_in_line() {
        let line = build_status_line(ConnectionState::Reconnecting, "gamma", None, None, None, 80);
        assert!(line_text(&line).contains("●"));
    }

    // ---- Indicator colours ----------------------------------------------

    #[test]
    fn connected_indicator_has_green_colour() {
        let line = build_status_line(ConnectionState::Connected, "p", None, None, None, 80);
        let dot = line.spans.iter().find(|s| s.content.contains('●')).unwrap();
        assert_eq!(
            dot.style.fg,
            Some(Color::Green),
            "Connected indicator must be green"
        );
    }

    #[test]
    fn disconnected_indicator_has_red_colour() {
        let line = build_status_line(ConnectionState::Disconnected, "p", None, None, None, 80);
        let dot = line.spans.iter().find(|s| s.content.contains('●')).unwrap();
        assert_eq!(
            dot.style.fg,
            Some(Color::Red),
            "Disconnected indicator must be red"
        );
    }

    #[test]
    fn reconnecting_indicator_has_yellow_colour() {
        let line = build_status_line(ConnectionState::Reconnecting, "p", None, None, None, 80);
        let dot = line.spans.iter().find(|s| s.content.contains('●')).unwrap();
        assert_eq!(
            dot.style.fg,
            Some(Color::Yellow),
            "Reconnecting indicator must be yellow"
        );
    }

    // ---- Space name ---------------------------------------------------

    #[test]
    fn space_name_always_appears() {
        let line = build_status_line(ConnectionState::Connected, "myspace", None, None, None, 80);
        assert!(
            line_text(&line).contains("myspace"),
            "space name must always be visible"
        );
    }

    // ---- Model name -----------------------------------------------------

    #[test]
    fn model_name_shown_when_provided() {
        let line = build_status_line(
            ConnectionState::Connected,
            "proj",
            Some("claude-sonnet"),
            None,
            None,
            80,
        );
        assert!(
            line_text(&line).contains("claude-sonnet"),
            "model name should appear when provided"
        );
    }

    #[test]
    fn model_name_shows_em_dash_when_none() {
        let line = build_status_line(ConnectionState::Connected, "proj", None, None, None, 80);
        assert!(
            line_text(&line).contains('—'),
            "em-dash expected when model name is None"
        );
    }

    // ---- Token counts ---------------------------------------------------

    #[test]
    fn token_counts_shown_when_both_provided() {
        let line = build_status_line(
            ConnectionState::Connected,
            "proj",
            Some("model"),
            Some(1024),
            Some(256),
            80,
        );
        let text = line_text(&line);
        assert!(
            text.contains("in: 1024"),
            "input token count missing; got: {text:?}"
        );
        assert!(
            text.contains("out: 256"),
            "output token count missing; got: {text:?}"
        );
    }

    #[test]
    fn token_counts_show_em_dash_when_none() {
        // When tokens are None the right section should still show a "—"
        // in place of the counts (via the model │ — pattern).
        let line = build_status_line(
            ConnectionState::Connected,
            "proj",
            Some("model"),
            None,
            None,
            80,
        );
        let text = line_text(&line);
        assert!(
            text.contains('—'),
            "em-dash expected for missing token counts; got: {text:?}"
        );
    }

    // ---- Graceful truncation --------------------------------------------

    #[test]
    fn narrow_terminal_drops_token_counts() {
        // Width 20: left=" ● proj " (8) leaves only 12 cols for right.
        // Full right "claude │ in: 10000 / out: 5000 " is >12 cols.
        // Model-only "claude " (7) fits; full right does not.
        let line = build_status_line(
            ConnectionState::Connected,
            "proj",
            Some("claude"),
            Some(10000),
            Some(5000),
            20,
        );
        let text = line_text(&line);
        assert!(
            !text.contains("in: 10000"),
            "token counts should be dropped on narrow terminal; got: {text:?}"
        );
    }

    #[test]
    fn very_narrow_terminal_drops_model_too() {
        // Width 10: left=" ● p " (5) leaves only 5 cols.
        // Even model-only "very-long-model " exceeds that.
        let line = build_status_line(
            ConnectionState::Connected,
            "p",
            Some("very-long-model"),
            Some(100),
            Some(50),
            10,
        );
        let text = line_text(&line);
        assert!(
            !text.contains("very-long-model"),
            "model should be dropped on very narrow terminal; got: {text:?}"
        );
    }

    #[test]
    fn indicator_still_present_on_very_narrow_terminal() {
        let line = build_status_line(
            ConnectionState::Reconnecting,
            "p",
            Some("longmodel"),
            Some(999),
            Some(888),
            5,
        );
        assert!(
            line_text(&line).contains('●'),
            "connection indicator must survive extreme truncation"
        );
    }

    // ---- Render smoke tests ---------------------------------------------

    #[test]
    fn render_status_bar_does_not_panic() {
        use ratatui::{Terminal, backend::TestBackend};
        let backend = TestBackend::new(80, 1);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                render_status_bar(
                    frame,
                    frame.area(),
                    ConnectionState::Connected,
                    "swage",
                    Some("claude-sonnet"),
                    Some(512),
                    Some(128),
                );
            })
            .unwrap();
    }

    #[test]
    fn render_status_bar_shows_space_name_in_buffer() {
        use ratatui::{Terminal, backend::TestBackend};
        let backend = TestBackend::new(80, 1);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                render_status_bar(
                    frame,
                    frame.area(),
                    ConnectionState::Connected,
                    "testspace",
                    None,
                    None,
                    None,
                );
            })
            .unwrap();
        let content: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(
            content.contains("testspace"),
            "rendered buffer should contain space name; got: {content:?}"
        );
    }

    #[test]
    fn render_status_bar_disconnected_state_shows_indicator() {
        use ratatui::{Terminal, backend::TestBackend};
        let backend = TestBackend::new(80, 1);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                render_status_bar(
                    frame,
                    frame.area(),
                    ConnectionState::Disconnected,
                    "proj",
                    None,
                    None,
                    None,
                );
            })
            .unwrap();
        let content: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(
            content.contains('●'),
            "rendered buffer must contain connection indicator; got: {content:?}"
        );
    }
}

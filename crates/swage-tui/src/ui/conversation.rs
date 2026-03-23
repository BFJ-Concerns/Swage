//! Conversation area widget — message history with streaming response and
//! tool call indicators.
//!
//! Renders the scrollable message list with role labels, progressive streaming
//! tokens for the active response, and inline tool call status indicators.

use ratatui::{
    layout::{Alignment, Rect},
    style::{Color, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Paragraph, Wrap},
    Frame,
};

use crate::state::{ActiveTool, DisplayMessage, DisplayToolCall};

/// Return a styled role label line for the given role string.
///
/// "user" → cyan "You:", "assistant" → green "Assistant:", anything else
/// falls back to a plain "<role>:" label so unknown roles render gracefully.
fn role_label(role: &str) -> Line<'static> {
    match role {
        "user" => Line::from(Span::styled("You:", Style::default().fg(Color::Cyan))),
        "assistant" => {
            Line::from(Span::styled("Assistant:", Style::default().fg(Color::Green)))
        }
        other => Line::from(Span::raw(format!("{other}:"))),
    }
}

/// Format a completed tool call as a coloured inline indicator.
///
/// Success: `[name: success Nms]` in green (duration shown when available).
/// Error:   `[name: error]` in red.
fn format_tool_call_line(tc: &DisplayToolCall) -> Line<'static> {
    let (label, colour) = match tc.status.as_str() {
        "success" => {
            let text = match tc.duration_ms {
                Some(ms) => format!("[{}: success {}ms]", tc.name, ms),
                None => format!("[{}: success]", tc.name),
            };
            (text, Color::Green)
        }
        // Any non-success status is shown as an error.
        _ => (format!("[{}: error]", tc.name), Color::Red),
    };
    Line::from(Span::styled(label, Style::default().fg(colour)))
}

/// Format an in-progress tool call: `[name: running]` in yellow.
fn format_active_tool_line(at: &ActiveTool) -> Line<'static> {
    Line::from(Span::styled(
        format!("[{}: running]", at.name),
        Style::default().fg(Color::Yellow),
    ))
}

/// Collect all display lines for a single message.
///
/// Layout: role label → content lines → completed tool call indicators →
/// blank separator. Each content line is emitted as a separate `Line` so
/// ratatui's word-wrap can operate on it independently.
fn message_lines(msg: &DisplayMessage) -> Vec<Line<'static>> {
    let mut lines = Vec::new();

    lines.push(role_label(&msg.role));

    for text_line in msg.content.lines() {
        lines.push(Line::from(Span::raw(text_line.to_owned())));
    }

    for tc in &msg.tool_calls {
        lines.push(format_tool_call_line(tc));
    }

    // Blank line between messages for visual breathing room.
    lines.push(Line::from(""));

    lines
}

/// Render the conversation area into `area`.
///
/// When there are no messages and no streaming is in progress, a centred
/// placeholder is shown instead of an empty panel. Scroll offset is applied
/// via `Paragraph::scroll`; passing 0 shows the top of the history (the
/// caller's `scroll_down` methods clamp to keep the bottom visible).
pub fn render_conversation(
    frame: &mut Frame,
    area: Rect,
    messages: &[DisplayMessage],
    streaming_buffer: &str,
    is_streaming: bool,
    active_tools: &[ActiveTool],
    scroll_offset: usize,
) {
    let block = Block::default()
        .title("Conversation")
        .borders(Borders::ALL);

    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Empty state — nothing to show yet.
    if messages.is_empty() && !is_streaming {
        let placeholder = Paragraph::new("Send a message to start a conversation")
            .style(Style::default().fg(Color::DarkGray))
            .alignment(Alignment::Center);
        frame.render_widget(placeholder, inner);
        return;
    }

    // Build the full line list from committed message history.
    let mut lines: Vec<Line> = messages
        .iter()
        .flat_map(|m| message_lines(m))
        .collect();

    // Append the in-progress streaming response.
    if is_streaming {
        lines.push(role_label("assistant"));
        // A block cursor at the end signals to the user that output is live.
        let with_cursor = format!("{streaming_buffer}▌");
        for text_line in with_cursor.lines() {
            lines.push(Line::from(Span::raw(text_line.to_owned())));
        }
        // Active tool indicators appear below the streaming text.
        for at in active_tools {
            lines.push(format_active_tool_line(at));
        }
    }

    let text = Text::from(lines);
    let paragraph = Paragraph::new(text)
        .wrap(Wrap { trim: false })
        .scroll((scroll_offset as u16, 0));
    frame.render_widget(paragraph, inner);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{ActiveTool, DisplayMessage, DisplayToolCall};
    use chrono::Utc;

    fn user_msg(content: &str) -> DisplayMessage {
        DisplayMessage {
            role: "user".to_string(),
            content: content.to_string(),
            timestamp: Utc::now(),
            tool_calls: vec![],
        }
    }

    fn assistant_msg(content: &str, tool_calls: Vec<DisplayToolCall>) -> DisplayMessage {
        DisplayMessage {
            role: "assistant".to_string(),
            content: content.to_string(),
            timestamp: Utc::now(),
            tool_calls,
        }
    }

    // --- role_label ---

    #[test]
    fn role_label_user_is_cyan() {
        let line = role_label("user");
        assert_eq!(line.spans[0].content, "You:");
        assert_eq!(line.spans[0].style.fg, Some(Color::Cyan));
    }

    #[test]
    fn role_label_assistant_is_green() {
        let line = role_label("assistant");
        assert_eq!(line.spans[0].content, "Assistant:");
        assert_eq!(line.spans[0].style.fg, Some(Color::Green));
    }

    #[test]
    fn role_label_unknown_does_not_panic() {
        let line = role_label("system");
        assert!(!line.spans.is_empty());
    }

    // --- format_tool_call_line ---

    #[test]
    fn tool_call_success_with_duration() {
        let tc = DisplayToolCall {
            name: "read_file".to_string(),
            status: "success".to_string(),
            duration_ms: Some(42),
        };
        let line = format_tool_call_line(&tc);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains("read_file"), "got: {text}");
        assert!(text.contains("42ms"), "got: {text}");
        assert_eq!(line.spans[0].style.fg, Some(Color::Green));
    }

    #[test]
    fn tool_call_success_without_duration() {
        let tc = DisplayToolCall {
            name: "search".to_string(),
            status: "success".to_string(),
            duration_ms: None,
        };
        let line = format_tool_call_line(&tc);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains("search"), "got: {text}");
        assert!(text.contains("success"), "got: {text}");
        assert_eq!(line.spans[0].style.fg, Some(Color::Green));
    }

    #[test]
    fn tool_call_error_is_red() {
        let tc = DisplayToolCall {
            name: "bash".to_string(),
            status: "error".to_string(),
            duration_ms: None,
        };
        let line = format_tool_call_line(&tc);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains("bash"), "got: {text}");
        assert!(text.contains("error"), "got: {text}");
        assert_eq!(line.spans[0].style.fg, Some(Color::Red));
    }

    // --- format_active_tool_line ---

    #[test]
    fn active_tool_shows_running_in_yellow() {
        let at = ActiveTool {
            call_id: "abc".to_string(),
            name: "search".to_string(),
        };
        let line = format_active_tool_line(&at);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains("search"), "got: {text}");
        assert!(text.contains("running"), "got: {text}");
        assert_eq!(line.spans[0].style.fg, Some(Color::Yellow));
    }

    // --- message_lines ---

    #[test]
    fn message_lines_starts_with_role_label() {
        let msg = user_msg("hello world");
        let lines = message_lines(&msg);
        let first_text: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(first_text.contains("You:"), "got: {first_text}");
    }

    #[test]
    fn message_lines_includes_content() {
        let msg = user_msg("hello world");
        let lines = message_lines(&msg);
        let all_text: String = lines[1..]
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref().to_string()))
            .collect();
        assert!(all_text.contains("hello world"), "got: {all_text}");
    }

    #[test]
    fn message_lines_includes_tool_calls() {
        let tc = DisplayToolCall {
            name: "read_file".to_string(),
            status: "success".to_string(),
            duration_ms: Some(10),
        };
        let msg = assistant_msg("done", vec![tc]);
        let lines = message_lines(&msg);
        let all_text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref().to_string()))
            .collect();
        assert!(all_text.contains("read_file"), "got: {all_text}");
    }

    #[test]
    fn message_lines_ends_with_blank_separator() {
        let msg = user_msg("hi");
        let lines = message_lines(&msg);
        let last = lines.last().unwrap();
        let text: String = last.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.is_empty(), "expected blank last line, got: {text:?}");
    }
}

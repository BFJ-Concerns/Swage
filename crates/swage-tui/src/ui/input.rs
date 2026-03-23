//! Input area widget — message composition with single-line and multi-line
//! modes.
//!
//! Renders the text input area at the bottom of the terminal with cursor
//! positioning, multi-line toggle (Ctrl+E), and a disabled visual state when
//! the gateway is disconnected.

use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Paragraph, Wrap},
};

/// Text input state — text buffer, cursor, and mode flags.
///
/// Cursor position is tracked in char units (not bytes), so multi-byte
/// characters are handled correctly. A cursor_position of 0 means "before
/// the first character"; cursor_position == text.chars().count() means
/// "after the last character".
#[derive(Debug, Default)]
pub struct InputState {
    pub text: String,
    /// Cursor position in char units.
    pub cursor_position: usize,
    pub is_multi_line: bool,
}

impl InputState {
    /// Create an empty, single-line `InputState`.
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert `c` at the current cursor position and advance the cursor.
    pub fn insert_char(&mut self, c: char) {
        let byte_idx = self.char_to_byte_idx(self.cursor_position);
        self.text.insert(byte_idx, c);
        self.cursor_position += 1;
    }

    /// Remove the character immediately before the cursor (backspace).
    ///
    /// No-op when the cursor is already at position 0.
    pub fn delete_char(&mut self) {
        if self.cursor_position == 0 {
            return;
        }
        // Find the byte offset just before the cursor, then the start of the
        // preceding character within that prefix.
        let byte_idx = self.char_to_byte_idx(self.cursor_position);
        let prev_byte = self.text[..byte_idx]
            .char_indices()
            .next_back()
            .map(|(i, _)| i)
            .unwrap_or(0);
        self.text.remove(prev_byte);
        self.cursor_position -= 1;
    }

    /// Move the cursor one character to the left, clamping at 0.
    pub fn move_cursor_left(&mut self) {
        self.cursor_position = self.cursor_position.saturating_sub(1);
    }

    /// Move the cursor one character to the right, clamping at end of text.
    pub fn move_cursor_right(&mut self) {
        let max = self.text.chars().count();
        if self.cursor_position < max {
            self.cursor_position += 1;
        }
    }

    /// Insert a newline at the current cursor position.
    ///
    /// Primarily used in multi-line mode (Ctrl+E).
    pub fn insert_newline(&mut self) {
        self.insert_char('\n');
    }

    /// Clear the text buffer and reset the cursor to the start.
    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor_position = 0;
    }

    /// Consume the current text and reset to an empty state.
    ///
    /// Returns the previous content. Used when submitting a message so the
    /// text is taken rather than cloned.
    pub fn take_text(&mut self) -> String {
        let content = std::mem::take(&mut self.text);
        self.cursor_position = 0;
        content
    }

    /// Return `true` if the text buffer is empty.
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    // ---- Internal helpers ------------------------------------------------

    /// Convert a char-unit cursor position to a byte offset in `self.text`.
    ///
    /// If `char_pos` is past the end of the string (e.g., equal to
    /// `chars().count()`), returns `text.len()` — the byte position just
    /// after the last character.
    fn char_to_byte_idx(&self, char_pos: usize) -> usize {
        self.text
            .char_indices()
            .nth(char_pos)
            .map(|(i, _)| i)
            .unwrap_or(self.text.len())
    }
}

// ---- Rendering -----------------------------------------------------------

/// Render the text input area into `area`.
///
/// Shows a bordered block titled "Message" (or "Message [multi-line]").
/// In disabled state the block is greyed out with an italic "Disconnected"
/// placeholder and no cursor is shown. In normal state the terminal cursor
/// is positioned at `input_state.cursor_position`.
pub fn render_input(frame: &mut Frame, area: Rect, input_state: &InputState, is_disabled: bool) {
    let title = if input_state.is_multi_line {
        " Message [multi-line] "
    } else {
        " Message "
    };

    let (border_style, content_style) = if is_disabled {
        (
            Style::default().fg(Color::DarkGray),
            Style::default().fg(Color::DarkGray),
        )
    } else {
        (Style::default(), Style::default())
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(border_style);

    // Compute the inner rect before consuming the block.
    let inner = block.inner(area);

    let content: Text = if is_disabled {
        // Grey italic placeholder — visually distinct from active input.
        Text::from(Line::from(Span::styled(
            "Disconnected",
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::ITALIC),
        )))
    } else if input_state.is_multi_line {
        // Split on explicit newlines; ratatui handles wrapping within each line.
        let lines: Vec<Line> = input_state
            .text
            .split('\n')
            .map(|l| Line::from(l.to_string()))
            .collect();
        Text::from(lines)
    } else {
        Text::from(input_state.text.as_str())
    };

    let mut paragraph = Paragraph::new(content)
        .block(block)
        .style(content_style);

    if input_state.is_multi_line && !is_disabled {
        // Word-wrap long lines in multi-line mode.
        paragraph = paragraph.wrap(Wrap { trim: false });
    }

    frame.render_widget(paragraph, area);

    // Position the terminal cursor when the input is active.
    if !is_disabled && inner.width > 0 && inner.height > 0 {
        let (row, col) = cursor_row_col(input_state);
        let x = inner.x.saturating_add(col as u16);
        let y = inner.y.saturating_add(row as u16);
        // Guard: only show the cursor if it falls within the visible inner area.
        if x < inner.x + inner.width && y < inner.y + inner.height {
            frame.set_cursor_position((x, y));
        }
    }
}

/// Compute the (row, col) of the cursor within the text content.
///
/// For single-line mode `row` is always 0. For multi-line mode `row` is the
/// logical line index and `col` is the char offset within that line. This
/// does not account for word-wrap reflow — it reflects the underlying logical
/// structure rather than the rendered layout.
// TODO Cursor position ignores word-wrap — hidden or misplaced on long lines
// priority: normal | tags: correctness, review:branch-era-02-delivery
// `cursor_row_col` returns logical (row, col) without accounting for wrapping applied
// by the `Wrap { trim: false }` `Paragraph`. When a typed line exceeds the widget
// width, the col coordinate overruns the visible area, placing the terminal cursor
// outside the widget or beyond the right edge of the screen.
// Fix: compute the rendered (row, col) by dividing the logical col by the inner
// widget width to find the wrapped row offset, as ratatui applies hard wrapping.
// ---
fn cursor_row_col(input_state: &InputState) -> (usize, usize) {
    if !input_state.is_multi_line {
        return (0, input_state.cursor_position);
    }

    let mut remaining = input_state.cursor_position;
    for (row, line) in input_state.text.split('\n').enumerate() {
        let line_len = line.chars().count();
        if remaining <= line_len {
            return (row, remaining);
        }
        // +1 to account for the '\n' character that split consumed.
        remaining -= line_len + 1;
    }

    // Cursor is past the end of the last line — shouldn't occur in normal
    // usage since cursor_position is always <= text.chars().count(), but
    // clamp gracefully.
    let total_lines = input_state.text.split('\n').count();
    (total_lines.saturating_sub(1), remaining)
}

// ---- Tests ---------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ---- insert_char --------------------------------------------------------

    #[test]
    fn insert_char_appends_to_empty_text() {
        let mut state = InputState::new();
        state.insert_char('h');
        state.insert_char('i');
        assert_eq!(state.text, "hi");
        assert_eq!(state.cursor_position, 2);
    }

    #[test]
    fn insert_char_inserts_at_cursor_not_always_at_end() {
        // "helo" with cursor at 3 (after "hel") — inserting 'l' should give "hello".
        let mut state = InputState::new();
        state.text = "helo".to_string();
        state.cursor_position = 3;
        state.insert_char('l');
        assert_eq!(state.text, "hello");
        assert_eq!(state.cursor_position, 4);
    }

    #[test]
    fn insert_char_at_start_of_non_empty_text() {
        let mut state = InputState::new();
        state.text = "world".to_string();
        state.cursor_position = 0;
        state.insert_char('!');
        assert_eq!(state.text, "!world");
        assert_eq!(state.cursor_position, 1);
    }

    #[test]
    fn insert_char_handles_multibyte_unicode() {
        let mut state = InputState::new();
        state.insert_char('é'); // 2-byte UTF-8
        state.insert_char('ñ'); // 2-byte UTF-8
        assert_eq!(state.text, "éñ");
        assert_eq!(state.cursor_position, 2);
    }

    // ---- delete_char --------------------------------------------------------

    #[test]
    fn delete_char_removes_character_before_cursor() {
        let mut state = InputState::new();
        state.text = "hello".to_string();
        state.cursor_position = 3; // after "hel"
        state.delete_char();
        assert_eq!(state.text, "helo");
        assert_eq!(state.cursor_position, 2);
    }

    #[test]
    fn delete_char_at_start_is_noop() {
        let mut state = InputState::new();
        state.text = "hello".to_string();
        state.cursor_position = 0;
        state.delete_char();
        assert_eq!(state.text, "hello");
        assert_eq!(state.cursor_position, 0);
    }

    #[test]
    fn delete_char_on_empty_text_is_noop() {
        let mut state = InputState::new();
        state.delete_char();
        assert_eq!(state.text, "");
        assert_eq!(state.cursor_position, 0);
    }

    #[test]
    fn delete_char_handles_multibyte_unicode() {
        let mut state = InputState::new();
        // "héllo" — 'é' occupies 2 bytes but is one char unit.
        state.text = "héllo".to_string();
        state.cursor_position = 2; // after "hé"
        state.delete_char();
        assert_eq!(state.text, "hllo");
        assert_eq!(state.cursor_position, 1);
    }

    // ---- cursor movement ----------------------------------------------------

    #[test]
    fn move_cursor_left_decrements_position() {
        let mut state = InputState::new();
        state.text = "hello".to_string();
        state.cursor_position = 3;
        state.move_cursor_left();
        assert_eq!(state.cursor_position, 2);
    }

    #[test]
    fn move_cursor_left_clamps_at_zero() {
        let mut state = InputState::new();
        state.text = "hello".to_string();
        state.cursor_position = 0;
        state.move_cursor_left();
        assert_eq!(state.cursor_position, 0);
    }

    #[test]
    fn move_cursor_right_increments_position() {
        let mut state = InputState::new();
        state.text = "hello".to_string();
        state.cursor_position = 2;
        state.move_cursor_right();
        assert_eq!(state.cursor_position, 3);
    }

    #[test]
    fn move_cursor_right_clamps_at_text_end() {
        let mut state = InputState::new();
        state.text = "hello".to_string();
        state.cursor_position = 5; // already at end
        state.move_cursor_right();
        assert_eq!(state.cursor_position, 5);
    }

    // ---- insert_newline -----------------------------------------------------

    #[test]
    fn insert_newline_adds_newline_at_cursor_position() {
        let mut state = InputState::new();
        state.text = "helloworld".to_string();
        state.cursor_position = 5; // between "hello" and "world"
        state.insert_newline();
        assert_eq!(state.text, "hello\nworld");
        assert_eq!(state.cursor_position, 6);
    }

    // ---- clear --------------------------------------------------------------

    #[test]
    fn clear_empties_text_and_resets_cursor() {
        let mut state = InputState::new();
        state.text = "some content".to_string();
        state.cursor_position = 5;
        state.clear();
        assert_eq!(state.text, "");
        assert_eq!(state.cursor_position, 0);
    }

    // ---- take_text ----------------------------------------------------------

    #[test]
    fn take_text_returns_content_and_resets_state() {
        let mut state = InputState::new();
        state.text = "send me".to_string();
        state.cursor_position = 7;
        let taken = state.take_text();
        assert_eq!(taken, "send me");
        assert_eq!(state.text, "");
        assert_eq!(state.cursor_position, 0);
    }

    #[test]
    fn take_text_on_empty_returns_empty_and_leaves_cursor_at_zero() {
        let mut state = InputState::new();
        let taken = state.take_text();
        assert_eq!(taken, "");
        assert_eq!(state.cursor_position, 0);
    }

    // ---- is_empty -----------------------------------------------------------

    #[test]
    fn is_empty_true_for_new_state() {
        assert!(InputState::new().is_empty());
    }

    #[test]
    fn is_empty_false_after_insert() {
        let mut state = InputState::new();
        state.insert_char('a');
        assert!(!state.is_empty());
    }

    // ---- cursor_row_col -----------------------------------------------------

    #[test]
    fn cursor_row_col_single_line_always_row_zero() {
        let state = InputState {
            text: "hello".to_string(),
            cursor_position: 3,
            is_multi_line: false,
        };
        assert_eq!(cursor_row_col(&state), (0, 3));
    }

    #[test]
    fn cursor_row_col_multiline_first_line() {
        let state = InputState {
            text: "hello\nworld".to_string(),
            cursor_position: 3, // in "hello", column 3
            is_multi_line: true,
        };
        assert_eq!(cursor_row_col(&state), (0, 3));
    }

    #[test]
    fn cursor_row_col_multiline_second_line() {
        let state = InputState {
            text: "hello\nworld".to_string(),
            cursor_position: 7, // 5 chars + '\n' + 1 into "world"
            is_multi_line: true,
        };
        assert_eq!(cursor_row_col(&state), (1, 1));
    }

    #[test]
    fn cursor_row_col_after_trailing_newline() {
        // "hi\n" — cursor at position 3 is on the empty line after the newline.
        let state = InputState {
            text: "hi\n".to_string(),
            cursor_position: 3,
            is_multi_line: true,
        };
        assert_eq!(cursor_row_col(&state), (1, 0));
    }

    // ---- render tests -------------------------------------------------------

    #[test]
    fn render_input_does_not_panic_with_normal_state() {
        use ratatui::{backend::TestBackend, Terminal};
        let backend = TestBackend::new(40, 5);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = InputState::new();
        state.insert_char('h');
        state.insert_char('i');
        terminal
            .draw(|frame| render_input(frame, frame.area(), &state, false))
            .unwrap();
    }

    #[test]
    fn render_input_does_not_panic_when_disabled() {
        use ratatui::{backend::TestBackend, Terminal};
        let backend = TestBackend::new(40, 5);
        let mut terminal = Terminal::new(backend).unwrap();
        let state = InputState::new();
        terminal
            .draw(|frame| render_input(frame, frame.area(), &state, true))
            .unwrap();
    }

    #[test]
    fn render_input_does_not_panic_in_multiline_mode() {
        use ratatui::{backend::TestBackend, Terminal};
        let backend = TestBackend::new(40, 10);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = InputState::new();
        state.is_multi_line = true;
        state.text = "line one\nline two".to_string();
        state.cursor_position = 17;
        terminal
            .draw(|frame| render_input(frame, frame.area(), &state, false))
            .unwrap();
    }

    #[test]
    fn render_input_normal_shows_message_title() {
        use ratatui::{backend::TestBackend, Terminal};
        let backend = TestBackend::new(40, 5);
        let mut terminal = Terminal::new(backend).unwrap();
        let state = InputState::new();
        terminal
            .draw(|frame| render_input(frame, frame.area(), &state, false))
            .unwrap();
        let content: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(
            content.contains("Message"),
            "normal input should contain 'Message' title; got: {content:?}"
        );
    }

    #[test]
    fn render_input_multiline_shows_multiline_title() {
        use ratatui::{backend::TestBackend, Terminal};
        let backend = TestBackend::new(40, 10);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = InputState::new();
        state.is_multi_line = true;
        terminal
            .draw(|frame| render_input(frame, frame.area(), &state, false))
            .unwrap();
        let content: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(
            content.contains("multi-line"),
            "multi-line input should contain 'multi-line' title; got: {content:?}"
        );
    }

    #[test]
    fn render_input_disabled_shows_disconnected_placeholder() {
        use ratatui::{backend::TestBackend, Terminal};
        let backend = TestBackend::new(40, 5);
        let mut terminal = Terminal::new(backend).unwrap();
        let state = InputState::new();
        terminal
            .draw(|frame| render_input(frame, frame.area(), &state, true))
            .unwrap();
        let content: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(
            content.contains("Disconnected"),
            "disabled input should show 'Disconnected'; got: {content:?}"
        );
    }
}

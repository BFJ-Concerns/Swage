//! Keyboard input mapping to application actions.
//!
//! Translates crossterm key events into semantic `AppAction` values that the
//! app event loop can dispatch without knowing the specific key bindings.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// Semantic actions the TUI can perform in response to user input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppAction {
    /// Send the current input as a message.
    SendMessage,
    /// Insert a newline in multi-line mode.
    InsertNewline,
    /// Insert a character at the cursor position.
    InsertChar(char),
    /// Delete the character before the cursor.
    DeleteChar,
    /// Move cursor left.
    MoveCursorLeft,
    /// Move cursor right.
    MoveCursorRight,
    /// Toggle multi-line input mode.
    ToggleMultiLine,
    /// Scroll conversation history up by one line.
    ScrollUp,
    /// Scroll conversation history up by one page.
    ScrollPageUp,
    /// Scroll conversation history down by one line.
    ScrollDown,
    /// Scroll conversation history down by one page.
    ScrollPageDown,
    /// Open the bench selector overlay.
    OpenBenchSelector,
    /// Toggle the context inspector panel.
    ToggleInspector,
    /// Start a new conversation.
    NewConversation,
    /// Quit the TUI.
    Quit,
}

/// Map a crossterm key event to a semantic application action.
///
/// `is_multi_line` controls mode-dependent bindings — Enter sends the
/// message in single-line mode but inserts a newline in multi-line mode.
/// Returns `None` for keys that have no binding.
pub fn map_key_event(event: KeyEvent, is_multi_line: bool) -> Option<AppAction> {
    let KeyEvent {
        code, modifiers, ..
    } = event;

    // Ctrl bindings are checked first so that Ctrl+letter combos never fall
    // through to the InsertChar arm below.
    if modifiers == KeyModifiers::CONTROL {
        return match code {
            KeyCode::Char('e') => Some(AppAction::ToggleMultiLine),
            KeyCode::Char('p') => Some(AppAction::OpenBenchSelector),
            KeyCode::Char('i') => Some(AppAction::ToggleInspector),
            KeyCode::Char('c') | KeyCode::Char('q') => Some(AppAction::Quit),
            KeyCode::Char('n') => Some(AppAction::NewConversation),
            _ => None,
        };
    }

    // Unmodified key bindings.
    match code {
        KeyCode::Enter => {
            if is_multi_line {
                Some(AppAction::InsertNewline)
            } else {
                Some(AppAction::SendMessage)
            }
        }
        KeyCode::Up => Some(AppAction::ScrollUp),
        KeyCode::PageUp => Some(AppAction::ScrollPageUp),
        KeyCode::Down => Some(AppAction::ScrollDown),
        KeyCode::PageDown => Some(AppAction::ScrollPageDown),
        KeyCode::Backspace => Some(AppAction::DeleteChar),
        KeyCode::Left => Some(AppAction::MoveCursorLeft),
        KeyCode::Right => Some(AppAction::MoveCursorRight),
        // Only insert plain characters — modified chars (e.g. Alt+x) are ignored.
        KeyCode::Char(c) if modifiers.is_empty() => Some(AppAction::InsertChar(c)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    // Helpers for building key events without boilerplate.
    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::CONTROL)
    }

    #[test]
    fn app_action_variants_exist() {
        // Verify the enum compiles with all expected variants.
        let actions = vec![
            AppAction::SendMessage,
            AppAction::InsertNewline,
            AppAction::InsertChar('a'),
            AppAction::DeleteChar,
            AppAction::MoveCursorLeft,
            AppAction::MoveCursorRight,
            AppAction::ToggleMultiLine,
            AppAction::ScrollUp,
            AppAction::ScrollPageUp,
            AppAction::ScrollDown,
            AppAction::ScrollPageDown,
            AppAction::OpenBenchSelector,
            AppAction::ToggleInspector,
            AppAction::NewConversation,
            AppAction::Quit,
        ];
        assert_eq!(actions.len(), 15);
    }

    // --- Enter key (mode-dependent) ---

    #[test]
    fn enter_single_line_sends_message() {
        assert_eq!(
            map_key_event(key(KeyCode::Enter), false),
            Some(AppAction::SendMessage)
        );
    }

    #[test]
    fn enter_multi_line_inserts_newline() {
        assert_eq!(
            map_key_event(key(KeyCode::Enter), true),
            Some(AppAction::InsertNewline)
        );
    }

    // --- Ctrl bindings ---

    #[test]
    fn ctrl_e_toggles_multi_line_in_both_modes() {
        assert_eq!(
            map_key_event(ctrl(KeyCode::Char('e')), false),
            Some(AppAction::ToggleMultiLine)
        );
        assert_eq!(
            map_key_event(ctrl(KeyCode::Char('e')), true),
            Some(AppAction::ToggleMultiLine)
        );
    }

    #[test]
    fn ctrl_p_opens_bench_selector() {
        assert_eq!(
            map_key_event(ctrl(KeyCode::Char('p')), false),
            Some(AppAction::OpenBenchSelector)
        );
    }

    #[test]
    fn ctrl_i_toggles_inspector() {
        assert_eq!(
            map_key_event(ctrl(KeyCode::Char('i')), false),
            Some(AppAction::ToggleInspector)
        );
    }

    #[test]
    fn ctrl_c_quits() {
        assert_eq!(
            map_key_event(ctrl(KeyCode::Char('c')), false),
            Some(AppAction::Quit)
        );
    }

    #[test]
    fn ctrl_q_quits() {
        assert_eq!(
            map_key_event(ctrl(KeyCode::Char('q')), false),
            Some(AppAction::Quit)
        );
    }

    #[test]
    fn ctrl_n_starts_new_conversation() {
        assert_eq!(
            map_key_event(ctrl(KeyCode::Char('n')), false),
            Some(AppAction::NewConversation)
        );
    }

    // --- Navigation keys ---

    #[test]
    fn up_arrow_scrolls_up() {
        assert_eq!(
            map_key_event(key(KeyCode::Up), false),
            Some(AppAction::ScrollUp)
        );
    }

    #[test]
    fn page_up_scrolls_page_up() {
        assert_eq!(
            map_key_event(key(KeyCode::PageUp), false),
            Some(AppAction::ScrollPageUp)
        );
    }

    #[test]
    fn down_arrow_scrolls_down() {
        assert_eq!(
            map_key_event(key(KeyCode::Down), false),
            Some(AppAction::ScrollDown)
        );
    }

    #[test]
    fn page_down_scrolls_page_down() {
        assert_eq!(
            map_key_event(key(KeyCode::PageDown), false),
            Some(AppAction::ScrollPageDown)
        );
    }

    // --- Editing keys ---

    #[test]
    fn backspace_deletes_char() {
        assert_eq!(
            map_key_event(key(KeyCode::Backspace), false),
            Some(AppAction::DeleteChar)
        );
    }

    #[test]
    fn left_arrow_moves_cursor_left() {
        assert_eq!(
            map_key_event(key(KeyCode::Left), false),
            Some(AppAction::MoveCursorLeft)
        );
    }

    #[test]
    fn right_arrow_moves_cursor_right() {
        assert_eq!(
            map_key_event(key(KeyCode::Right), false),
            Some(AppAction::MoveCursorRight)
        );
    }

    // --- Character insertion ---

    #[test]
    fn printable_char_inserts_char() {
        assert_eq!(
            map_key_event(key(KeyCode::Char('a')), false),
            Some(AppAction::InsertChar('a'))
        );
        assert_eq!(
            map_key_event(key(KeyCode::Char('Z')), false),
            Some(AppAction::InsertChar('Z'))
        );
        assert_eq!(
            map_key_event(key(KeyCode::Char('!')), false),
            Some(AppAction::InsertChar('!'))
        );
        assert_eq!(
            map_key_event(key(KeyCode::Char(' ')), false),
            Some(AppAction::InsertChar(' '))
        );
    }

    // --- Unknown keys ---

    #[test]
    fn unknown_keys_return_none() {
        assert_eq!(map_key_event(key(KeyCode::F(1)), false), None);
        assert_eq!(map_key_event(key(KeyCode::Home), false), None);
        assert_eq!(map_key_event(key(KeyCode::End), false), None);
        assert_eq!(map_key_event(key(KeyCode::Delete), false), None);
    }

    // --- Ctrl chars do not fall through to InsertChar ---

    #[test]
    fn ctrl_char_does_not_insert() {
        // Ctrl+A is not mapped — should return None, not InsertChar('a').
        assert_eq!(map_key_event(ctrl(KeyCode::Char('a')), false), None);
    }
}

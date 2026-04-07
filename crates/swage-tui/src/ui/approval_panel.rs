//! Approval panel widget — displays pending HITL approvals with
//! keyboard-driven approve/reject actions.
//!
//! The panel is hidden when no approvals are pending. It renders as an
//! overlay at the bottom of the conversation area, showing the most
//! recent pending approval with its tool/hook context, prompt, and
//! countdown timer.

use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
};

use uuid::Uuid;

/// A pending approval visible in the TUI.
#[derive(Debug, Clone)]
pub struct PendingApproval {
    pub id: Uuid,
    pub turn_id: Uuid,
    pub agent_id: String,
    pub kind: String,
    pub tool_name: Option<String>,
    pub prompt: String,
    pub timeout_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// State for the approval panel.
#[derive(Debug, Default)]
pub struct ApprovalPanelState {
    /// Currently pending approvals, ordered by creation time.
    pub pending: Vec<PendingApproval>,
    /// Index of the currently focused approval (for multi-approval navigation).
    pub selected_index: usize,
}

impl ApprovalPanelState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether the panel should be visible.
    pub fn is_visible(&self) -> bool {
        !self.pending.is_empty()
    }

    /// Add a new pending approval.
    pub fn add(&mut self, approval: PendingApproval) {
        self.pending.push(approval);
    }

    /// Remove an approval by ID (after decision or expiry).
    pub fn remove(&mut self, id: Uuid) {
        self.pending.retain(|a| a.id != id);
        // Adjust selected index if needed.
        if self.selected_index >= self.pending.len() && !self.pending.is_empty() {
            self.selected_index = self.pending.len() - 1;
        }
    }

    /// Clear all pending approvals.
    pub fn clear(&mut self) {
        self.pending.clear();
        self.selected_index = 0;
    }

    /// Get the currently focused approval.
    pub fn focused(&self) -> Option<&PendingApproval> {
        self.pending.get(self.selected_index)
    }
}

/// Render the approval panel in the given area.
///
/// The panel is a no-op when `state.is_visible()` is false. When visible,
/// it renders a bordered box with the approval prompt, context, and
/// keyboard hints for approve (Enter/y) and reject (Escape/n).
pub fn render_approval_panel(frame: &mut Frame, area: Rect, state: &ApprovalPanelState) {
    if !state.is_visible() {
        return;
    }

    let Some(approval) = state.focused() else {
        return;
    };

    let title = format!(
        " Approval Required ({}/{}) ",
        state.selected_index + 1,
        state.pending.len()
    );

    // Build the content lines.
    let mut lines = Vec::new();

    // Tool/hook context.
    let context_text = if let Some(ref tool) = approval.tool_name {
        format!("{}: {}", approval.kind, tool)
    } else {
        approval.kind.clone()
    };
    lines.push(Line::from(vec![
        Span::styled("Context: ", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(context_text),
    ]));

    // Agent ID.
    lines.push(Line::from(vec![
        Span::styled("Agent:   ", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(&approval.agent_id),
    ]));

    // Timeout countdown.
    if let Some(timeout_at) = approval.timeout_at {
        let remaining = timeout_at - chrono::Utc::now();
        let secs = remaining.num_seconds().max(0);
        let timeout_str = if secs > 60 {
            format!("{}m {}s", secs / 60, secs % 60)
        } else {
            format!("{secs}s")
        };
        lines.push(Line::from(vec![
            Span::styled("Timeout: ", Style::default().add_modifier(Modifier::BOLD)),
            Span::styled(timeout_str, Style::default().fg(Color::Yellow)),
        ]));
    }

    // Blank separator.
    lines.push(Line::from(""));

    // Prompt text.
    lines.push(Line::from(vec![
        Span::styled("Prompt: ", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(&approval.prompt),
    ]));

    // Blank separator.
    lines.push(Line::from(""));

    // Keyboard hints.
    lines.push(Line::from(vec![
        Span::styled(
            "  [y/Enter] ",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("Approve  "),
        Span::styled(
            "  [n/Esc] ",
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        ),
        Span::raw("Reject"),
    ]));

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Yellow))
        .title(title);

    let paragraph = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false });

    frame.render_widget(paragraph, area);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_approval() -> PendingApproval {
        PendingApproval {
            id: Uuid::new_v4(),
            turn_id: Uuid::new_v4(),
            agent_id: "gateway".into(),
            kind: "tool".into(),
            tool_name: Some("shell_execute".into()),
            prompt: "Allow shell_execute with command 'rm -rf /'?".into(),
            timeout_at: Some(chrono::Utc::now() + chrono::Duration::seconds(300)),
        }
    }

    #[test]
    fn empty_panel_not_visible() {
        let state = ApprovalPanelState::new();
        assert!(!state.is_visible());
    }

    #[test]
    fn panel_visible_with_pending() {
        let mut state = ApprovalPanelState::new();
        state.add(sample_approval());
        assert!(state.is_visible());
    }

    #[test]
    fn remove_clears_correct_entry() {
        let mut state = ApprovalPanelState::new();
        let a = sample_approval();
        let id = a.id;
        state.add(a);
        state.add(sample_approval());
        assert_eq!(state.pending.len(), 2);

        state.remove(id);
        assert_eq!(state.pending.len(), 1);
        assert_ne!(state.pending[0].id, id);
    }

    #[test]
    fn selected_index_adjusts_on_remove() {
        let mut state = ApprovalPanelState::new();
        state.add(sample_approval());
        state.add(sample_approval());
        state.selected_index = 1;

        state.remove(state.pending[1].id);
        assert_eq!(state.selected_index, 0);
    }

    #[test]
    fn focused_returns_selected() {
        let mut state = ApprovalPanelState::new();
        let a = sample_approval();
        let id = a.id;
        state.add(a);
        assert_eq!(state.focused().unwrap().id, id);
    }

    #[test]
    fn render_does_not_panic() {
        use ratatui::{Terminal, backend::TestBackend};
        let backend = TestBackend::new(80, 12);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = ApprovalPanelState::new();
        state.add(sample_approval());
        terminal
            .draw(|frame| {
                render_approval_panel(frame, frame.area(), &state);
            })
            .unwrap();
    }

    #[test]
    fn render_noop_when_empty() {
        use ratatui::{Terminal, backend::TestBackend};
        let backend = TestBackend::new(80, 12);
        let mut terminal = Terminal::new(backend).unwrap();
        let state = ApprovalPanelState::new();
        terminal
            .draw(|frame| {
                render_approval_panel(frame, frame.area(), &state);
            })
            .unwrap();
    }
}

//! Conversation state model — the single source of truth for what the TUI
//! displays.
//!
//! Holds the message history, streaming token buffer, active tool calls,
//! scroll position, turn metadata, context assembly report, and project info.
//! All state mutations flow through methods on `ConversationState` so the
//! rendering layer can read a consistent snapshot.

use chrono::{DateTime, Utc};
use uuid::Uuid;

/// A message ready for display in the conversation area.
#[derive(Debug, Clone)]
pub struct DisplayMessage {
    pub role: String,
    pub content: String,
    pub timestamp: DateTime<Utc>,
    pub tool_calls: Vec<DisplayToolCall>,
}

/// A completed tool call shown inline with its parent message.
#[derive(Debug, Clone)]
pub struct DisplayToolCall {
    pub name: String,
    pub status: String,
    pub duration_ms: Option<u64>,
}

/// A tool call currently in progress.
#[derive(Debug, Clone)]
pub struct ActiveTool {
    pub call_id: String,
    pub name: String,
}

/// Parsed context assembly report for the inspector panel.
#[derive(Debug, Clone)]
pub struct ContextReport {
    pub sections: Vec<ContextSection>,
    pub total_tokens: u64,
    pub budget_tokens: Option<u64>,
}

/// A single section in the context assembly report.
#[derive(Debug, Clone)]
pub struct ContextSection {
    pub name: String,
    pub token_count: u64,
}

/// Minimal project info for display in the status bar.
#[derive(Debug, Clone)]
pub struct ProjectInfo {
    pub id: Uuid,
    pub name: String,
}

// ---------------------------------------------------------------------------
// ConversationState
// ---------------------------------------------------------------------------

/// The single source of truth for everything the TUI displays.
///
/// All state changes go through methods here — the rendering layer reads
/// fields directly but never mutates them.
pub struct ConversationState {
    /// Committed messages from the conversation history.
    messages: Vec<DisplayMessage>,
    /// Tokens arriving during the current streaming turn.
    streaming_buffer: String,
    /// Tool calls currently in progress (awaiting `end_tool`).
    active_tools: Vec<ActiveTool>,
    /// Scroll position within the conversation area (line index from top).
    scroll_offset: usize,
    /// Latest context assembly report from the gateway, if any.
    pub context_report: Option<ContextReport>,
    /// Project this conversation belongs to.
    pub active_project: ProjectInfo,
    /// Model name from the most recently completed turn.
    pub model_name: Option<String>,
    /// Input token count from the most recently completed turn.
    pub input_tokens: Option<u32>,
    /// Output token count from the most recently completed turn.
    pub output_tokens: Option<u32>,
    /// True while a streaming turn is in progress.
    is_streaming: bool,
    /// Gateway conversation ID, set when a conversation is opened or resumed.
    pub conversation_id: Option<Uuid>,
    /// Tool calls completed during the current turn, queued for `complete_turn`
    /// to flush into the new `DisplayMessage`.
    completed_tools: Vec<DisplayToolCall>,
}

impl ConversationState {
    /// Creates a new state anchored to the given project.
    ///
    /// All conversational fields start empty; scroll is at the top.
    pub fn new(project: ProjectInfo) -> Self {
        Self {
            messages: Vec::new(),
            streaming_buffer: String::new(),
            active_tools: Vec::new(),
            scroll_offset: 0,
            context_report: None,
            active_project: project,
            model_name: None,
            input_tokens: None,
            output_tokens: None,
            is_streaming: false,
            conversation_id: None,
            completed_tools: Vec::new(),
        }
    }

    pub fn messages(&self) -> &[DisplayMessage] {
        &self.messages
    }

    pub fn streaming_buffer(&self) -> &str {
        &self.streaming_buffer
    }

    pub fn active_tools(&self) -> &[ActiveTool] {
        &self.active_tools
    }

    pub fn scroll_offset(&self) -> usize {
        self.scroll_offset
    }

    pub fn is_streaming(&self) -> bool {
        self.is_streaming
    }

    pub fn conversation_id(&self) -> Option<Uuid> {
        self.conversation_id
    }

    pub fn set_conversation_id(&mut self, conversation_id: Option<Uuid>) {
        self.conversation_id = conversation_id;
    }

    pub fn push_local_message(&mut self, role: impl Into<String>, content: impl Into<String>) {
        self.messages.push(DisplayMessage {
            role: role.into(),
            content: content.into(),
            timestamp: Utc::now(),
            tool_calls: vec![],
        });
    }

    pub fn cancel_streaming(&mut self) {
        self.streaming_buffer.clear();
        self.active_tools.clear();
        self.completed_tools.clear();
        self.is_streaming = false;
    }

    /// Appends a streaming token to the buffer and marks the turn as active.
    pub fn append_token(&mut self, text: &str) {
        self.streaming_buffer.push_str(text);
        self.is_streaming = true;
    }

    /// Records that a tool call has started.
    ///
    /// The tool appears in `active_tools` until `end_tool` is called with the
    /// same `call_id`.
    pub fn start_tool(&mut self, call_id: String, name: String) {
        self.active_tools.push(ActiveTool { call_id, name });
    }

    /// Records that an in-progress tool call has finished.
    ///
    /// Removes the entry from `active_tools`, creates a `DisplayToolCall`, and
    /// queues it for the next `complete_turn`. Unknown call IDs are silently
    /// ignored — out-of-order gateway events are not fatal.
    pub fn end_tool(&mut self, call_id: &str, status: String, duration_ms: u64) {
        if let Some(pos) = self.active_tools.iter().position(|t| t.call_id == call_id) {
            let tool = self.active_tools.remove(pos);
            self.completed_tools.push(DisplayToolCall {
                name: tool.name,
                status,
                duration_ms: Some(duration_ms),
            });
        }
    }

    /// Commits the current streaming turn as a `DisplayMessage`.
    ///
    /// The streaming buffer becomes the message content; all completed tool
    /// calls are attached. Any tools still marked active (no `end_tool` event
    /// received) are flushed with `"unknown"` status so the history stays
    /// coherent even after an unexpected disconnect.
    pub fn complete_turn(
        &mut self,
        model: Option<String>,
        input_tokens: Option<u32>,
        output_tokens: Option<u32>,
    ) {
        // Gather completed tools, then flush any still-active ones.
        let mut tool_calls = std::mem::take(&mut self.completed_tools);
        for t in self.active_tools.drain(..) {
            tool_calls.push(DisplayToolCall {
                name: t.name,
                status: "unknown".to_string(),
                duration_ms: None,
            });
        }

        let content = std::mem::take(&mut self.streaming_buffer);
        self.messages.push(DisplayMessage {
            role: "assistant".to_string(),
            content,
            timestamp: Utc::now(),
            tool_calls,
        });

        self.is_streaming = false;
        self.model_name = model;
        self.input_tokens = input_tokens;
        self.output_tokens = output_tokens;
    }

    /// Replaces the context inspector data with a fresh report from the gateway.
    pub fn update_context_report(&mut self, report: ContextReport) {
        self.context_report = Some(report);
    }

    /// Switches the active project.
    pub fn set_project(&mut self, project: ProjectInfo) {
        self.active_project = project;
    }

    /// Replaces the message history with records loaded from storage.
    ///
    /// Call this once when resuming an existing conversation, before any
    /// streaming events arrive.
    pub fn load_history(&mut self, messages: Vec<DisplayMessage>) {
        self.messages = messages;
    }

    /// Scrolls the view up by `amount` lines, clamping at the top.
    pub fn scroll_up(&mut self, amount: usize) {
        self.scroll_offset = self.scroll_offset.saturating_sub(amount);
    }

    /// Scrolls the view down by `amount` lines, clamping at the bottom of
    /// content.
    ///
    /// `visible_height` is the number of lines the viewport can show; it is
    /// used to prevent scrolling past the last line of content.
    pub fn scroll_down(&mut self, amount: usize, visible_width: usize, visible_height: usize) {
        if visible_width == 0 || visible_height == 0 {
            return;
        }

        let content_lines = self.total_rendered_lines(visible_width);
        let max_offset = content_lines.saturating_sub(visible_height);
        self.scroll_offset = (self.scroll_offset + amount).min(max_offset);
    }

    /// Resets all conversational state while preserving project affiliation and
    /// conversation identity.
    ///
    /// Clears: messages, streaming buffer, active/completed tools, scroll
    /// position, token counts, model name, context report, and streaming flag.
    /// Does NOT clear `active_project` or `conversation_id`.
    pub fn clear(&mut self) {
        self.messages.clear();
        self.streaming_buffer.clear();
        self.active_tools.clear();
        self.completed_tools.clear();
        self.scroll_offset = 0;
        self.context_report = None;
        self.model_name = None;
        self.input_tokens = None;
        self.output_tokens = None;
        self.is_streaming = false;
    }

    fn total_rendered_lines(&self, visible_width: usize) -> usize {
        let mut total = self
            .messages
            .iter()
            .map(|message| rendered_message_lines(message, visible_width))
            .sum();

        if self.is_streaming {
            total += wrapped_line_count(&role_label_text("assistant"), visible_width);
            let streaming_with_cursor = format!("{}▌", self.streaming_buffer);
            for line in streaming_with_cursor.lines() {
                total += wrapped_line_count(line, visible_width);
            }
            for tool in &self.active_tools {
                total += wrapped_line_count(&active_tool_text(tool), visible_width);
            }
        }

        total
    }
}

fn rendered_message_lines(message: &DisplayMessage, visible_width: usize) -> usize {
    let mut total = wrapped_line_count(&role_label_text(&message.role), visible_width);

    for line in message.content.lines() {
        total += wrapped_line_count(line, visible_width);
    }

    for tool_call in &message.tool_calls {
        total += wrapped_line_count(&tool_call_text(tool_call), visible_width);
    }

    // Blank separator between messages.
    total + 1
}

fn wrapped_line_count(text: &str, visible_width: usize) -> usize {
    if visible_width == 0 {
        return 0;
    }

    let char_count = text.chars().count().max(1);
    (char_count + visible_width - 1) / visible_width
}

fn role_label_text(role: &str) -> String {
    match role {
        "user" => "You:".to_string(),
        "assistant" => "Assistant:".to_string(),
        other => format!("{other}:"),
    }
}

fn tool_call_text(tool_call: &DisplayToolCall) -> String {
    match tool_call.status.as_str() {
        "success" => match tool_call.duration_ms {
            Some(ms) => format!("[{}: success {}ms]", tool_call.name, ms),
            None => format!("[{}: success]", tool_call.name),
        },
        _ => format!("[{}: error]", tool_call.name),
    }
}

fn active_tool_text(tool: &ActiveTool) -> String {
    format!("[{}: running]", tool.name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_project() -> ProjectInfo {
        ProjectInfo {
            id: Uuid::new_v4(),
            name: "Test Project".to_string(),
        }
    }

    // --- Existing scaffolded types ---

    #[test]
    fn display_message_can_be_constructed() {
        let msg = DisplayMessage {
            role: "user".to_string(),
            content: "hello".to_string(),
            timestamp: Utc::now(),
            tool_calls: vec![],
        };
        assert_eq!(msg.role, "user");
    }

    #[test]
    fn context_report_holds_sections() {
        let report = ContextReport {
            sections: vec![ContextSection {
                name: "history".to_string(),
                token_count: 500,
            }],
            total_tokens: 500,
            budget_tokens: Some(4096),
        };
        assert_eq!(report.sections.len(), 1);
        assert_eq!(report.total_tokens, 500);
    }

    // --- ConversationState::new ---

    #[test]
    fn new_initialises_empty_state() {
        let project = default_project();
        let project_id = project.id;
        let state = ConversationState::new(project);

        assert!(state.messages.is_empty());
        assert!(state.streaming_buffer.is_empty());
        assert!(state.active_tools.is_empty());
        assert_eq!(state.scroll_offset, 0);
        assert!(state.context_report.is_none());
        assert_eq!(state.active_project.id, project_id);
        assert!(state.model_name.is_none());
        assert!(state.input_tokens.is_none());
        assert!(state.output_tokens.is_none());
        assert!(!state.is_streaming);
        assert!(state.conversation_id.is_none());
    }

    // --- append_token ---

    #[test]
    fn append_token_adds_to_buffer_and_sets_streaming() {
        let mut state = ConversationState::new(default_project());
        state.append_token("Hello");
        assert_eq!(state.streaming_buffer, "Hello");
        assert!(state.is_streaming);
    }

    #[test]
    fn append_token_accumulates_across_calls() {
        let mut state = ConversationState::new(default_project());
        state.append_token("foo");
        state.append_token(" bar");
        assert_eq!(state.streaming_buffer, "foo bar");
    }

    // --- start_tool / end_tool ---

    #[test]
    fn start_tool_adds_to_active_tools() {
        let mut state = ConversationState::new(default_project());
        state.start_tool("call-1".to_string(), "read_file".to_string());
        assert_eq!(state.active_tools.len(), 1);
        assert_eq!(state.active_tools[0].call_id, "call-1");
        assert_eq!(state.active_tools[0].name, "read_file");
    }

    #[test]
    fn end_tool_removes_from_active_tools() {
        let mut state = ConversationState::new(default_project());
        state.start_tool("call-1".to_string(), "read_file".to_string());
        state.end_tool("call-1", "success".to_string(), 42);
        assert!(state.active_tools.is_empty());
    }

    #[test]
    fn end_tool_ignores_unknown_call_id() {
        let mut state = ConversationState::new(default_project());
        state.start_tool("call-1".to_string(), "read_file".to_string());
        state.end_tool("call-99", "success".to_string(), 0);
        assert_eq!(state.active_tools.len(), 1);
    }

    #[test]
    fn cancel_streaming_clears_buffers_and_tool_state() {
        let mut state = ConversationState::new(default_project());
        state.append_token("partial");
        state.start_tool("call-1".to_string(), "shell".to_string());
        state.end_tool("call-1", "success".to_string(), 42);

        state.cancel_streaming();

        assert!(state.streaming_buffer.is_empty());
        assert!(state.active_tools.is_empty());
        assert!(state.completed_tools.is_empty());
        assert!(!state.is_streaming);
    }

    // --- complete_turn ---

    #[test]
    fn complete_turn_flushes_buffer_into_message() {
        let mut state = ConversationState::new(default_project());
        state.append_token("Hello from the LLM.");
        state.complete_turn(Some("claude-3".to_string()), Some(10), Some(5));
        assert_eq!(state.messages.len(), 1);
        assert_eq!(state.messages[0].role, "assistant");
        assert_eq!(state.messages[0].content, "Hello from the LLM.");
        assert!(state.streaming_buffer.is_empty());
        assert!(!state.is_streaming);
    }

    #[test]
    fn complete_turn_records_token_counts_and_model() {
        let mut state = ConversationState::new(default_project());
        state.append_token("response");
        state.complete_turn(Some("sonnet-4".to_string()), Some(100), Some(50));
        assert_eq!(state.model_name.as_deref(), Some("sonnet-4"));
        assert_eq!(state.input_tokens, Some(100));
        assert_eq!(state.output_tokens, Some(50));
    }

    #[test]
    fn complete_turn_attaches_completed_tools_to_message() {
        let mut state = ConversationState::new(default_project());
        state.start_tool("call-1".to_string(), "shell".to_string());
        state.end_tool("call-1", "success".to_string(), 123);
        state.append_token("Done.");
        state.complete_turn(None, None, None);
        let msg = &state.messages[0];
        assert_eq!(msg.tool_calls.len(), 1);
        assert_eq!(msg.tool_calls[0].name, "shell");
        assert_eq!(msg.tool_calls[0].status, "success");
        assert_eq!(msg.tool_calls[0].duration_ms, Some(123));
    }

    #[test]
    fn complete_turn_flushes_still_active_tools_as_unknown() {
        let mut state = ConversationState::new(default_project());
        state.start_tool("call-1".to_string(), "slow_tool".to_string());
        // end_tool never called — turn completes while tool is still running.
        state.append_token("Partial.");
        state.complete_turn(None, None, None);
        assert!(state.active_tools.is_empty());
        let msg = &state.messages[0];
        assert_eq!(msg.tool_calls.len(), 1);
        assert_eq!(msg.tool_calls[0].name, "slow_tool");
        assert_eq!(msg.tool_calls[0].status, "unknown");
    }

    // --- update_context_report ---

    #[test]
    fn update_context_report_stores_report() {
        let mut state = ConversationState::new(default_project());
        let report = ContextReport {
            sections: vec![ContextSection {
                name: "system".to_string(),
                token_count: 200,
            }],
            total_tokens: 200,
            budget_tokens: Some(4096),
        };
        state.update_context_report(report);
        assert!(state.context_report.is_some());
        assert_eq!(state.context_report.as_ref().unwrap().total_tokens, 200);
    }

    // --- set_project ---

    #[test]
    fn set_project_updates_active_project() {
        let mut state = ConversationState::new(default_project());
        let new_project = ProjectInfo {
            id: Uuid::new_v4(),
            name: "New Project".to_string(),
        };
        let new_id = new_project.id;
        state.set_project(new_project);
        assert_eq!(state.active_project.id, new_id);
        assert_eq!(state.active_project.name, "New Project");
    }

    // --- load_history ---

    #[test]
    fn load_history_replaces_messages() {
        let mut state = ConversationState::new(default_project());
        state.append_token("existing token");
        let history = vec![DisplayMessage {
            role: "user".to_string(),
            content: "old message".to_string(),
            timestamp: Utc::now(),
            tool_calls: vec![],
        }];
        state.load_history(history);
        assert_eq!(state.messages.len(), 1);
        assert_eq!(state.messages[0].content, "old message");
    }

    // --- scroll_up / scroll_down ---

    #[test]
    fn scroll_up_decrements_offset() {
        let mut state = ConversationState::new(default_project());
        state.scroll_offset = 10;
        state.scroll_up(3);
        assert_eq!(state.scroll_offset, 7);
    }

    #[test]
    fn scroll_up_clamps_at_zero() {
        let mut state = ConversationState::new(default_project());
        state.scroll_offset = 2;
        state.scroll_up(10);
        assert_eq!(state.scroll_offset, 0);
    }

    #[test]
    fn scroll_down_increments_offset() {
        let mut state = ConversationState::new(default_project());
        for i in 0..20 {
            state.messages.push(DisplayMessage {
                role: "user".to_string(),
                content: format!("msg {i}"),
                timestamp: Utc::now(),
                tool_calls: vec![],
            });
        }
        state.scroll_down(3, 80, 5);
        assert_eq!(state.scroll_offset, 3);
    }

    #[test]
    fn scroll_down_clamps_at_content_end() {
        let mut state = ConversationState::new(default_project());
        // 10 short messages at width 80 render as 3 lines each
        // (role label + content + separator), so 30 total lines.
        // Viewport height 5 → max_offset = 25.
        for i in 0..10 {
            state.messages.push(DisplayMessage {
                role: "user".to_string(),
                content: format!("msg {i}"),
                timestamp: Utc::now(),
                tool_calls: vec![],
            });
        }
        state.scroll_down(100, 80, 5);
        assert_eq!(state.scroll_offset, 25);
    }

    #[test]
    fn scroll_down_accounts_for_wrapped_message_height() {
        let mut state = ConversationState::new(default_project());
        state.messages.push(DisplayMessage {
            role: "assistant".to_string(),
            content: "abcdefghij".to_string(),
            timestamp: Utc::now(),
            tool_calls: vec![],
        });

        state.scroll_down(10, 4, 3);

        assert_eq!(state.scroll_offset, 4);
    }

    // --- clear ---

    #[test]
    fn clear_resets_conversational_state_but_preserves_project() {
        let project = default_project();
        let project_id = project.id;
        let mut state = ConversationState::new(project);
        state.append_token("some text");
        state.start_tool("call-1".to_string(), "shell".to_string());
        state.scroll_offset = 5;
        let conv_id = Uuid::new_v4();
        state.conversation_id = Some(conv_id);
        state.messages.push(DisplayMessage {
            role: "user".to_string(),
            content: "hi".to_string(),
            timestamp: Utc::now(),
            tool_calls: vec![],
        });

        state.clear();

        assert!(state.messages.is_empty());
        assert!(state.streaming_buffer.is_empty());
        assert!(state.active_tools.is_empty());
        assert_eq!(state.scroll_offset, 0);
        assert!(state.context_report.is_none());
        assert!(!state.is_streaming);
        // Project and conversation identity are structural — preserved by clear.
        assert_eq!(state.active_project.id, project_id);
        assert_eq!(state.conversation_id, Some(conv_id));
    }
}

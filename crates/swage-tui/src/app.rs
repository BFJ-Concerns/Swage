//! Application struct, terminal lifecycle, and main event dispatch loop.
//!
//! `App` owns all TUI state and orchestrates the render/event cycle. It enters
//! crossterm raw mode on start and restores the terminal on exit — including
//! panic cleanup via a custom hook.

use crate::event::{AppEvent, EventHandler};
use crate::gateway::{ConnectionManager, ConnectionState};
use crate::keybindings::{AppAction, map_key_event};
use crate::state::{ContextReport, ContextSection, ConversationState, ProjectInfo};
use crate::ui::input::InputState;
use crate::ui::project_selector::ProjectSelectorState;
use crate::ui::{RenderParams, calculate_layout, compute_input_height, render};
use swage_client::error::ClientError;
use swage_client::websocket::ServerEvent;

#[derive(Debug, Clone, Copy)]
struct PendingReconnect {
    attempt: u32,
    next_attempt_at: tokio::time::Instant,
}

impl PendingReconnect {
    fn immediate() -> Self {
        Self {
            attempt: 0,
            next_attempt_at: tokio::time::Instant::now(),
        }
    }

    fn schedule_retry(&mut self) {
        let delay = crate::gateway::backoff_delay(self.attempt);
        self.attempt += 1;
        self.next_attempt_at = tokio::time::Instant::now() + delay;
    }
}

// ---------------------------------------------------------------------------
// App
// ---------------------------------------------------------------------------

/// All TUI state for a single session, plus the connection to the gateway.
///
/// `App` does not connect to the gateway on construction — call [`run`][App::run]
/// which enters the terminal lifecycle and attempts the first connection there.
pub struct App {
    /// Conversation history, streaming buffer, and context inspector data.
    pub state: ConversationState,
    /// Gateway HTTP + WebSocket connection, including reconnection logic.
    pub connection: ConnectionManager,
    /// Text input widget state (cursor, text, multi-line mode).
    pub input: InputState,
    /// Whether the context inspector panel is shown on the right.
    pub show_inspector: bool,
    /// Project selector modal overlay.
    pub project_selector: ProjectSelectorState,
    /// True while the application event loop should keep running.
    pub is_running: bool,
    pending_reconnect: Option<PendingReconnect>,
}

impl App {
    /// Create an `App` targeting `gateway_url` with `project` as the initial
    /// active project.
    ///
    /// Does not connect to the gateway — connection is established inside
    /// [`run`][App::run] once the terminal is set up.
    pub fn new(gateway_url: &str, project: ProjectInfo) -> Self {
        Self {
            state: ConversationState::new(project),
            connection: ConnectionManager::new(gateway_url.to_string()),
            input: InputState::new(),
            show_inspector: false,
            project_selector: ProjectSelectorState::new(),
            is_running: true,
            pending_reconnect: None,
        }
    }

    fn queue_reconnect(&mut self) {
        self.connection.begin_reconnect();
        self.pending_reconnect = Some(PendingReconnect::immediate());
    }

    fn conversation_viewport(
        &self,
        terminal_area: ratatui::layout::Rect,
    ) -> Option<(usize, usize)> {
        let input_height = compute_input_height(&self.input);
        let regions = calculate_layout(terminal_area, input_height, self.show_inspector)?;
        Some((
            regions.conversation_area.width.saturating_sub(2) as usize,
            regions.conversation_area.height.saturating_sub(2) as usize,
        ))
    }

    // --- Sync state mutations (no I/O) ------------------------------------

    /// Apply a synchronous action to the application state.
    ///
    /// Actions that require async I/O (`SendMessage`, `NewConversation`,
    /// project confirmation) or live viewport dimensions (scroll-down actions)
    /// are handled directly in the event loop and do not come through here.
    pub fn dispatch_action(&mut self, action: AppAction) {
        match action {
            AppAction::Quit => self.is_running = false,
            AppAction::ToggleInspector => self.show_inspector = !self.show_inspector,
            AppAction::InsertChar(c) => self.input.insert_char(c),
            AppAction::DeleteChar => self.input.delete_char(),
            AppAction::InsertNewline => self.input.insert_newline(),
            AppAction::MoveCursorLeft => self.input.move_cursor_left(),
            AppAction::MoveCursorRight => self.input.move_cursor_right(),
            AppAction::ToggleMultiLine => self.input.is_multi_line = !self.input.is_multi_line,
            AppAction::ScrollUp => self.state.scroll_up(1),
            AppAction::ScrollDown => {}
            // Use a larger step for page scroll — keeps some context visible.
            AppAction::ScrollPageUp => self.state.scroll_up(10),
            AppAction::ScrollPageDown => {}
            AppAction::NewConversation => {}
            AppAction::OpenProjectSelector => self.project_selector.show(),
            // SendMessage requires async I/O — handled in event_loop.
            AppAction::SendMessage => {}
        }
    }

    // --- Terminal lifecycle -----------------------------------------------

    /// Enter raw mode, run the event loop, and restore the terminal on exit.
    ///
    /// Attempts an initial connection to the gateway; displays an error in the
    /// conversation area if the gateway is unreachable but keeps running.
    /// On exit (quit action or unrecoverable error) the terminal is always
    /// restored to its original state — including on panic.
    pub async fn run(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        use crossterm::{
            event::{DisableMouseCapture, EnableMouseCapture},
            execute,
            terminal::{
                EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
            },
        };
        use ratatui::{Terminal, backend::CrosstermBackend};
        use std::io::stdout;

        enable_raw_mode()?;
        let mut out = stdout();
        execute!(out, EnterAlternateScreen, EnableMouseCapture)?;

        // Install a panic hook that restores the terminal before printing the
        // panic message. Without this, a crash leaves the terminal in raw mode
        // and makes the shell unusable.
        let default_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let _ = disable_raw_mode();
            let _ = execute!(std::io::stdout(), LeaveAlternateScreen, DisableMouseCapture,);
            default_hook(info);
        }));

        let backend = CrosstermBackend::new(stdout());
        let mut terminal = Terminal::new(backend)?;

        let result = self.event_loop(&mut terminal).await;

        // ---- Teardown — always runs, even on error ----
        // Use let _ = to suppress errors during cleanup; the primary result
        // from event_loop is what callers care about.
        let _ = disable_raw_mode();
        let _ = execute!(
            terminal.backend_mut(),
            LeaveAlternateScreen,
            DisableMouseCapture,
        );
        let _ = terminal.show_cursor();

        result
    }

    // --- Event loop (private) --------------------------------------------

    async fn event_loop(
        &mut self,
        terminal: &mut ratatui::Terminal<ratatui::backend::CrosstermBackend<std::io::Stdout>>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        use crossterm::event::{Event as TermEvent, KeyCode};
        use std::time::Duration;

        // EventHandler is a local — NOT a field on App — so that App::new()
        // does not require a TTY. The crossterm EventStream panics in
        // non-TTY environments; keeping it outside the constructor lets
        // tests call App::new() safely.
        let mut event_handler = EventHandler::new(Duration::from_millis(16));

        // Attempt an initial connection. Display an inline error if it fails
        // but keep running — the user can type once the gateway starts.
        if let Err(e) = self.connection.connect().await {
            self.state
                .push_local_message("error", format!("Gateway connection failed: {e}"));
            self.queue_reconnect();
        } else {
            self.state
                .set_conversation_id(self.connection.conversation_id());
        }

        while self.is_running {
            if let Some(reconnect) = self.pending_reconnect.as_mut() {
                if tokio::time::Instant::now() >= reconnect.next_attempt_at {
                    match self.connection.connect().await {
                        Ok(()) => {
                            self.pending_reconnect = None;
                            self.state
                                .set_conversation_id(self.connection.conversation_id());
                            self.state
                                .push_local_message("system", "Reconnected to gateway");
                        }
                        Err(error) => {
                            tracing::warn!(error = %error, attempt = reconnect.attempt, "gateway reconnect attempt failed");
                            self.connection.begin_reconnect();
                            reconnect.schedule_retry();
                        }
                    }
                }
            }

            // ---- Render the current frame ----
            let conn_state = self.connection.connection_state();
            terminal.draw(|frame| {
                let params = RenderParams {
                    state: &self.state,
                    input_state: &self.input,
                    selector_state: &self.project_selector,
                    connection_state: conn_state,
                    is_inspector_open: self.show_inspector,
                };
                render(frame, &params);
            })?;

            // ---- Multiplex terminal events and gateway events ----
            //
            // We pass `&mut None` to EventHandler (dormant gateway arm) and
            // handle gateway reads directly via `self.connection.next_event()`
            // in the second select arm. Using a local EventHandler avoids
            // a self-borrow conflict — event_handler is not part of self.
            let is_connected = conn_state == ConnectionState::Connected;
            let mut dormant_sink = None;
            tokio::select! {
                app_event = event_handler.next(&mut dormant_sink) => {
                    match app_event {
                        AppEvent::Terminal(TermEvent::Key(key)) => {
                            if self.project_selector.is_visible() {
                                // Route keys to the project selector overlay.
                                match key.code {
                                    KeyCode::Up => self.project_selector.select_previous(),
                                    KeyCode::Down => self.project_selector.select_next(),
                                    KeyCode::Enter => {
                                        if let Some(project) = self.project_selector.confirm() {
                                            self.project_selector.hide();
                                            self.connection.disconnect().await;
                                            match self.connection
                                                .connect_for_project(project.id)
                                                .await
                                            {
                                                Ok(()) => {
                                                    // Clear stale messages, tokens, and streaming state
                                                    // before switching projects so the display reflects the
                                                    // fresh session rather than the previous one.
                                                    self.state.clear();
                                                    self.state.set_project(project);
                                                    self.state.set_conversation_id(
                                                        self.connection.conversation_id(),
                                                    );
                                                    self.pending_reconnect = None;
                                                }
                                                Err(e) => {
                                                    self.state.push_local_message(
                                                        "error",
                                                        format!("Project switch failed: {e}"),
                                                    );
                                                }
                                            }
                                        }
                                    }
                                    KeyCode::Esc => self.project_selector.hide(),
                                    _ => {}
                                }
                            } else if let Some(action) =
                                map_key_event(key, self.input.is_multi_line)
                            {
                                // SendMessage requires async I/O — handle here rather
                                // than in the sync dispatch_action.
                                if action == AppAction::SendMessage {
                                    if !self.input.is_empty() {
                                        let text = self.input.take_text();
                                        self.state.push_local_message("user", text.clone());
                                        if let Err(e) =
                                            self.connection.send_message(text).await
                                        {
                                            self.state.push_local_message(
                                                "error",
                                                format!("Send failed: {e}"),
                                            );
                                        }
                                    }
                                } else if action == AppAction::NewConversation {
                                    match self.connection.start_new_conversation().await {
                                        Ok(()) => {
                                            self.pending_reconnect = None;
                                            self.state.clear();
                                            self.state.set_conversation_id(
                                                self.connection.conversation_id(),
                                            );
                                        }
                                        Err(e) => {
                                            self.state.push_local_message(
                                                "error",
                                                format!("New conversation failed: {e}"),
                                            );
                                        }
                                    }
                                } else if matches!(
                                    action,
                                    AppAction::ScrollDown | AppAction::ScrollPageDown
                                ) {
                                    if let Some((visible_width, visible_height)) =
                                        self.conversation_viewport({
                                            let size = terminal.size()?;
                                            ratatui::layout::Rect::new(0, 0, size.width, size.height)
                                        })
                                    {
                                        let amount = if action == AppAction::ScrollDown {
                                            1
                                        } else {
                                            visible_height.max(1)
                                        };
                                        self.state.scroll_down(
                                            amount,
                                            visible_width,
                                            visible_height,
                                        );
                                    }
                                } else if action == AppAction::ScrollPageUp {
                                    if let Some((_, visible_height)) =
                                        self.conversation_viewport({
                                            let size = terminal.size()?;
                                            ratatui::layout::Rect::new(0, 0, size.width, size.height)
                                        })
                                    {
                                        self.state.scroll_up(visible_height.max(1));
                                    }
                                } else {
                                    self.dispatch_action(action);
                                }
                            }
                        }
                        AppEvent::Terminal(TermEvent::Resize(_, _)) => {
                            // Ratatui picks up the new size automatically on the
                            // next draw call; no additional handling needed.
                        }
                        AppEvent::Tick => {
                            // Tick drives periodic redraws — the render at the top
                            // of the loop handles the actual draw.
                        }
                        _ => {}
                    }
                }

                gateway_result = async {
                    if is_connected {
                        self.connection.next_event().await
                    } else {
                        // Keep the gateway arm dormant when not connected so
                        // we don't spin on immediate errors.
                        std::future::pending::<Result<Option<ServerEvent>, ClientError>>()
                            .await
                    }
                } => {
                    match gateway_result {
                        Ok(Some(event)) => {
                            let outcome = handle_server_event(&mut self.state, event);
                            if outcome == EventOutcome::Shutdown {
                                self.connection.disconnect().await;
                            }
                        }
                        Ok(None) => {
                            // Clean WebSocket close — gateway disconnected.
                            self.connection.disconnect().await;
                            self.state
                                .push_local_message("system", "Connection lost — retrying…");
                            self.queue_reconnect();
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, "WebSocket error");
                            self.state.push_local_message(
                                "error",
                                format!("Connection error: {e}"),
                            );
                            self.connection.disconnect().await;
                            self.queue_reconnect();
                        }
                    }
                }
            }
        }

        self.connection.disconnect().await;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// EventOutcome
// ---------------------------------------------------------------------------

/// What the caller should do after handling a server event.
#[derive(Debug, PartialEq, Eq)]
pub enum EventOutcome {
    /// Normal event — no additional action required.
    Continue,
    /// The gateway is shutting down; the connection manager should close the
    /// WebSocket and show a reconnection prompt.
    Shutdown,
}

// ---------------------------------------------------------------------------
// handle_server_event
// ---------------------------------------------------------------------------

/// Dispatches a gateway `ServerEvent` to the appropriate `ConversationState`
/// mutation and returns an outcome the connection manager can act on.
///
/// This is a pure function of `state` + `event` — no I/O, no async.
/// The caller is responsible for re-rendering after this call.
pub fn handle_server_event(state: &mut ConversationState, event: ServerEvent) -> EventOutcome {
    match event {
        ServerEvent::Token { content } => {
            state.append_token(&content);
            EventOutcome::Continue
        }

        ServerEvent::ToolStart { call_id, tool, .. } => {
            state.start_tool(call_id, tool);
            EventOutcome::Continue
        }

        ServerEvent::ToolEnd {
            call_id,
            status,
            duration_ms,
            ..
        } => {
            state.end_tool(&call_id, status, duration_ms);
            EventOutcome::Continue
        }

        ServerEvent::TurnComplete {
            input_tokens,
            output_tokens,
            ..
        } => {
            // Model name is not carried by TurnComplete — it comes from
            // configuration or a ContextAssembled event. Pass None here; the
            // caller may set it separately once known.
            state.complete_turn(None, input_tokens, output_tokens);
            EventOutcome::Continue
        }

        ServerEvent::ContextAssembled { report, .. } => {
            let context_report = parse_context_report(&report);
            state.update_context_report(context_report);
            EventOutcome::Continue
        }

        ServerEvent::Shutdown { reason } => {
            state.push_local_message("system", format!("Gateway shutting down — {reason}"));
            EventOutcome::Shutdown
        }

        ServerEvent::Error { message, .. } => {
            state.push_local_message("error", message);
            EventOutcome::Continue
        }

        ServerEvent::Cancelled {} => {
            // The user cancelled the turn — discard any partial streamed content.
            state.cancel_streaming();
            EventOutcome::Continue
        }

        ServerEvent::SkillActivated { display_name, .. } => {
            state.push_local_message("system", format!("Skill activated: {display_name}"));
            EventOutcome::Continue
        }

        ServerEvent::SkillDeactivated { name } => {
            state.push_local_message("system", format!("Skill deactivated: {name}"));
            EventOutcome::Continue
        }

        // A no-op from the gateway's perspective — the skill was already
        // loaded so no state change is needed. Silently continue.
        ServerEvent::SkillAlreadyActive { .. } => EventOutcome::Continue,

        // Inter-agent communication events.
        ServerEvent::DelegationStarted {
            target_agent_id, ..
        } => {
            state.push_local_message("system", format!("Delegating to agent: {target_agent_id}"));
            EventOutcome::Continue
        }

        ServerEvent::DelegationCompleted {
            target_agent_id,
            success,
            ..
        } => {
            let status = if success { "succeeded" } else { "failed" };
            state.push_local_message(
                "system",
                format!("Delegation to {target_agent_id} {status}"),
            );
            EventOutcome::Continue
        }

        ServerEvent::ApprovalRequired { prompt, .. } => {
            state.push_local_message("system", format!("Approval required: {prompt}"));
            EventOutcome::Continue
        }

        ServerEvent::ApprovalConfirmed { decision, .. } => {
            state.push_local_message("system", format!("Approval {decision}"));
            EventOutcome::Continue
        }

        ServerEvent::ApprovalExpired { .. } => {
            state.push_local_message("system", "Approval request expired");
            EventOutcome::Continue
        }

        ServerEvent::ApprovalDecisionError { error, message, .. } => {
            state.push_local_message(
                "error",
                format!("Approval decision failed ({error}): {message}"),
            );
            EventOutcome::Continue
        }
    }
}

// ---------------------------------------------------------------------------
// Context report parsing
// ---------------------------------------------------------------------------

/// Parses an `AssemblyReport` JSON value (as sent by the gateway in
/// `ContextAssembled` events) into a `ContextReport` for the inspector panel.
///
/// Missing or malformed fields degrade gracefully — the display shows what
/// it can rather than crashing on an unexpected server response.
fn parse_context_report(report: &serde_json::Value) -> ContextReport {
    let total_tokens = report
        .get("total_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);

    let budget_tokens = report.get("budget").and_then(|v| v.as_u64());

    let sections = report
        .get("elements")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|el| {
                    let name = el.get("name")?.as_str()?.to_string();
                    let token_count = el.get("token_count")?.as_u64()?;
                    Some(ContextSection { name, token_count })
                })
                .collect()
        })
        .unwrap_or_default();

    ContextReport {
        sections,
        total_tokens,
        budget_tokens,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use uuid::Uuid;

    fn make_state() -> ConversationState {
        ConversationState::new(ProjectInfo {
            id: Uuid::new_v4(),
            name: "Test".to_string(),
        })
    }

    // --- Token ---

    #[test]
    fn token_event_appends_to_buffer_and_sets_streaming() {
        let mut state = make_state();
        let outcome = handle_server_event(
            &mut state,
            ServerEvent::Token {
                content: "hello".to_string(),
            },
        );
        assert_eq!(state.streaming_buffer(), "hello");
        assert!(state.is_streaming());
        assert_eq!(outcome, EventOutcome::Continue);
    }

    #[test]
    fn multiple_token_events_accumulate() {
        let mut state = make_state();
        handle_server_event(
            &mut state,
            ServerEvent::Token {
                content: "foo".to_string(),
            },
        );
        handle_server_event(
            &mut state,
            ServerEvent::Token {
                content: " bar".to_string(),
            },
        );
        assert_eq!(state.streaming_buffer(), "foo bar");
    }

    // --- ToolStart / ToolEnd ---

    #[test]
    fn tool_start_adds_active_tool() {
        let mut state = make_state();
        handle_server_event(
            &mut state,
            ServerEvent::ToolStart {
                call_id: "tc-1".to_string(),
                tool: "read_file".to_string(),
                arguments: json!({}),
                parent_call_id: None,
            },
        );
        assert_eq!(state.active_tools().len(), 1);
        assert_eq!(state.active_tools()[0].call_id, "tc-1");
        assert_eq!(state.active_tools()[0].name, "read_file");
    }

    #[test]
    fn tool_end_removes_active_tool() {
        let mut state = make_state();
        handle_server_event(
            &mut state,
            ServerEvent::ToolStart {
                call_id: "tc-1".to_string(),
                tool: "shell".to_string(),
                arguments: json!({}),
                parent_call_id: None,
            },
        );
        handle_server_event(
            &mut state,
            ServerEvent::ToolEnd {
                call_id: "tc-1".to_string(),
                tool: "shell".to_string(),
                result: json!("ok"),
                status: "success".to_string(),
                duration_ms: 77,
                truncated: false,
                total_bytes: None,
                parent_call_id: None,
            },
        );
        assert!(state.active_tools().is_empty());
    }

    // --- TurnComplete ---

    #[test]
    fn turn_complete_flushes_buffer_to_message() {
        let mut state = make_state();
        handle_server_event(
            &mut state,
            ServerEvent::Token {
                content: "Result.".to_string(),
            },
        );
        let turn_id = Uuid::new_v4();
        let outcome = handle_server_event(
            &mut state,
            ServerEvent::TurnComplete {
                turn_id,
                input_tokens: Some(50),
                output_tokens: Some(10),
                cost_microcents: None,
                structured_output: None,
            },
        );
        assert_eq!(state.messages().len(), 1);
        assert_eq!(state.messages()[0].content, "Result.");
        assert_eq!(state.messages()[0].role, "assistant");
        assert!(state.streaming_buffer().is_empty());
        assert!(!state.is_streaming());
        assert_eq!(state.input_tokens, Some(50));
        assert_eq!(state.output_tokens, Some(10));
        assert_eq!(outcome, EventOutcome::Continue);
    }

    // --- ContextAssembled ---

    #[test]
    fn context_assembled_parses_report_into_context_report() {
        let mut state = make_state();
        let report_json = json!({
            "assembly_name": "default",
            "elements": [
                { "name": "history", "source": "thread-1", "token_count": 300, "priority": 0 },
                { "name": "prompts", "source": "global", "token_count": 150, "priority": 100 }
            ],
            "total_tokens": 450,
            "budget": 4096,
            "budget_remaining": 3646,
            "compaction_applied": [],
            "warnings": [],
            "errors": []
        });
        handle_server_event(
            &mut state,
            ServerEvent::ContextAssembled {
                turn_id: Uuid::new_v4(),
                report: report_json,
            },
        );
        let report = state
            .context_report
            .as_ref()
            .expect("context_report should be set");
        assert_eq!(report.total_tokens, 450);
        assert_eq!(report.budget_tokens, Some(4096));
        assert_eq!(report.sections.len(), 2);
        assert_eq!(report.sections[0].name, "history");
        assert_eq!(report.sections[0].token_count, 300);
        assert_eq!(report.sections[1].name, "prompts");
        assert_eq!(report.sections[1].token_count, 150);
    }

    #[test]
    fn context_assembled_with_missing_fields_degrades_gracefully() {
        let mut state = make_state();
        // Minimal report: no elements, no budget.
        let report_json = json!({ "total_tokens": 99 });
        handle_server_event(
            &mut state,
            ServerEvent::ContextAssembled {
                turn_id: Uuid::new_v4(),
                report: report_json,
            },
        );
        let report = state.context_report.as_ref().unwrap();
        assert_eq!(report.total_tokens, 99);
        assert!(report.budget_tokens.is_none());
        assert!(report.sections.is_empty());
    }

    // --- Shutdown ---

    #[test]
    fn shutdown_adds_system_message_and_returns_shutdown() {
        let mut state = make_state();
        let outcome = handle_server_event(
            &mut state,
            ServerEvent::Shutdown {
                reason: "idle timeout".to_string(),
            },
        );
        assert_eq!(outcome, EventOutcome::Shutdown);
        assert_eq!(state.messages().len(), 1);
        assert_eq!(state.messages()[0].role, "system");
        assert!(state.messages()[0].content.contains("idle timeout"));
    }

    // --- Error ---

    #[test]
    fn error_event_adds_error_message() {
        let mut state = make_state();
        let outcome = handle_server_event(
            &mut state,
            ServerEvent::Error {
                code: "provider_error".to_string(),
                message: "API key missing".to_string(),
            },
        );
        assert_eq!(outcome, EventOutcome::Continue);
        assert_eq!(state.messages().len(), 1);
        assert_eq!(state.messages()[0].role, "error");
        assert_eq!(state.messages()[0].content, "API key missing");
    }

    // --- Cancelled ---

    #[test]
    fn cancelled_clears_streaming_buffer() {
        let mut state = make_state();
        handle_server_event(
            &mut state,
            ServerEvent::Token {
                content: "partial...".to_string(),
            },
        );
        assert!(state.is_streaming());
        let outcome = handle_server_event(&mut state, ServerEvent::Cancelled {});
        assert_eq!(outcome, EventOutcome::Continue);
        assert!(state.streaming_buffer().is_empty());
        assert!(!state.is_streaming());
    }

    // --- SkillActivated ---

    #[test]
    fn skill_activated_adds_system_message() {
        let mut state = make_state();
        let outcome = handle_server_event(
            &mut state,
            ServerEvent::SkillActivated {
                name: "my-skill".to_string(),
                display_name: "My Skill".to_string(),
                description: "Does something useful.".to_string(),
                token_count: 42,
            },
        );
        assert_eq!(outcome, EventOutcome::Continue);
        assert_eq!(state.messages().len(), 1);
        assert_eq!(state.messages()[0].role, "system");
        assert!(state.messages()[0].content.contains("My Skill"));
    }

    // --- SkillDeactivated ---

    #[test]
    fn skill_deactivated_adds_system_message() {
        let mut state = make_state();
        let outcome = handle_server_event(
            &mut state,
            ServerEvent::SkillDeactivated {
                name: "old-skill".to_string(),
            },
        );
        assert_eq!(outcome, EventOutcome::Continue);
        assert_eq!(state.messages().len(), 1);
        assert_eq!(state.messages()[0].role, "system");
        assert!(state.messages()[0].content.contains("old-skill"));
    }

    // --- Delegation and approval lifecycle ---

    #[test]
    fn delegation_started_adds_system_message() {
        let mut state = make_state();
        let outcome = handle_server_event(
            &mut state,
            ServerEvent::DelegationStarted {
                parent_turn_id: Uuid::new_v4(),
                child_turn_id: Uuid::new_v4(),
                target_agent_id: "reviewer".to_string(),
            },
        );
        assert_eq!(outcome, EventOutcome::Continue);
        assert_eq!(state.messages().len(), 1);
        assert_eq!(state.messages()[0].role, "system");
        assert!(state.messages()[0].content.contains("reviewer"));
    }

    #[test]
    fn delegation_completed_adds_success_message() {
        let mut state = make_state();
        let outcome = handle_server_event(
            &mut state,
            ServerEvent::DelegationCompleted {
                parent_turn_id: Uuid::new_v4(),
                child_turn_id: Uuid::new_v4(),
                target_agent_id: "reviewer".to_string(),
                success: true,
            },
        );
        assert_eq!(outcome, EventOutcome::Continue);
        assert_eq!(state.messages().len(), 1);
        assert_eq!(state.messages()[0].role, "system");
        assert!(state.messages()[0].content.contains("succeeded"));
    }

    #[test]
    fn approval_required_adds_system_message() {
        let mut state = make_state();
        let outcome = handle_server_event(
            &mut state,
            ServerEvent::ApprovalRequired {
                approval_id: Uuid::new_v4(),
                turn_id: Uuid::new_v4(),
                agent_id: "gateway".to_string(),
                kind: "tool".to_string(),
                tool_name: Some("shell".to_string()),
                tool_arguments: Some(json!({ "command": "ls" })),
                prompt: "Allow shell?".to_string(),
                context: Some(json!({ "cwd": "/tmp" })),
                timeout_at: "2026-01-01T00:00:00Z".to_string(),
                timeout_policy: "reject".to_string(),
            },
        );
        assert_eq!(outcome, EventOutcome::Continue);
        assert_eq!(state.messages().len(), 1);
        assert_eq!(state.messages()[0].role, "system");
        assert!(state.messages()[0].content.contains("Allow shell?"));
    }

    #[test]
    fn approval_confirmed_adds_system_message() {
        let mut state = make_state();
        let outcome = handle_server_event(
            &mut state,
            ServerEvent::ApprovalConfirmed {
                approval_id: Uuid::new_v4(),
                decision: "approved".to_string(),
                reason: Some("looks safe".to_string()),
            },
        );
        assert_eq!(outcome, EventOutcome::Continue);
        assert_eq!(state.messages().len(), 1);
        assert_eq!(state.messages()[0].role, "system");
        assert!(state.messages()[0].content.contains("approved"));
    }

    #[test]
    fn approval_expired_adds_system_message() {
        let mut state = make_state();
        let outcome = handle_server_event(
            &mut state,
            ServerEvent::ApprovalExpired {
                approval_id: Uuid::new_v4(),
                timeout_policy: "interrupt".to_string(),
            },
        );
        assert_eq!(outcome, EventOutcome::Continue);
        assert_eq!(state.messages().len(), 1);
        assert_eq!(state.messages()[0].role, "system");
        assert!(state.messages()[0].content.contains("expired"));
    }

    #[test]
    fn approval_decision_error_adds_error_message() {
        let mut state = make_state();
        let outcome = handle_server_event(
            &mut state,
            ServerEvent::ApprovalDecisionError {
                approval_id: "bad-id".to_string(),
                error: "already_decided".to_string(),
                message: "Approval was already resolved.".to_string(),
                current_state: Some("approved".to_string()),
            },
        );
        assert_eq!(outcome, EventOutcome::Continue);
        assert_eq!(state.messages().len(), 1);
        assert_eq!(state.messages()[0].role, "error");
        assert!(state.messages()[0].content.contains("already_decided"));
    }

    // --- App construction ---

    #[test]
    fn app_new_constructs_with_disconnected_state() {
        use crate::gateway::ConnectionState;
        let project = ProjectInfo {
            id: Uuid::new_v4(),
            name: "Test".to_string(),
        };
        let app = App::new("http://127.0.0.1:3000", project);
        assert_eq!(
            app.connection.connection_state(),
            ConnectionState::Disconnected
        );
        assert!(app.is_running);
        assert!(!app.show_inspector);
    }

    // --- dispatch_action ---

    #[test]
    fn dispatch_quit_sets_is_running_false() {
        use crate::keybindings::AppAction;
        let project = ProjectInfo {
            id: Uuid::new_v4(),
            name: "Test".to_string(),
        };
        let mut app = App::new("http://127.0.0.1:3000", project);
        app.dispatch_action(AppAction::Quit);
        assert!(!app.is_running);
    }

    #[test]
    fn dispatch_toggle_inspector_toggles_flag() {
        use crate::keybindings::AppAction;
        let project = ProjectInfo {
            id: Uuid::new_v4(),
            name: "Test".to_string(),
        };
        let mut app = App::new("http://127.0.0.1:3000", project);
        assert!(!app.show_inspector);
        app.dispatch_action(AppAction::ToggleInspector);
        assert!(app.show_inspector);
        app.dispatch_action(AppAction::ToggleInspector);
        assert!(!app.show_inspector);
    }

    #[test]
    fn dispatch_insert_char_updates_input() {
        use crate::keybindings::AppAction;
        let project = ProjectInfo {
            id: Uuid::new_v4(),
            name: "Test".to_string(),
        };
        let mut app = App::new("http://127.0.0.1:3000", project);
        app.dispatch_action(AppAction::InsertChar('h'));
        app.dispatch_action(AppAction::InsertChar('i'));
        assert_eq!(app.input.text, "hi");
    }

    #[test]
    fn dispatch_delete_char_removes_from_input() {
        use crate::keybindings::AppAction;
        let project = ProjectInfo {
            id: Uuid::new_v4(),
            name: "Test".to_string(),
        };
        let mut app = App::new("http://127.0.0.1:3000", project);
        app.dispatch_action(AppAction::InsertChar('x'));
        assert_eq!(app.input.text, "x");
        app.dispatch_action(AppAction::DeleteChar);
        assert!(app.input.text.is_empty());
    }

    #[test]
    fn dispatch_toggle_multi_line_toggles_mode() {
        use crate::keybindings::AppAction;
        let project = ProjectInfo {
            id: Uuid::new_v4(),
            name: "Test".to_string(),
        };
        let mut app = App::new("http://127.0.0.1:3000", project);
        assert!(!app.input.is_multi_line);
        app.dispatch_action(AppAction::ToggleMultiLine);
        assert!(app.input.is_multi_line);
    }

    #[test]
    fn dispatch_open_project_selector_shows_overlay() {
        use crate::keybindings::AppAction;
        let project = ProjectInfo {
            id: Uuid::new_v4(),
            name: "Test".to_string(),
        };
        let mut app = App::new("http://127.0.0.1:3000", project);
        assert!(!app.project_selector.is_visible());
        app.dispatch_action(AppAction::OpenProjectSelector);
        assert!(app.project_selector.is_visible());
    }

    #[test]
    fn dispatch_new_conversation_is_handled_in_event_loop() {
        use crate::keybindings::AppAction;
        let project = ProjectInfo {
            id: Uuid::new_v4(),
            name: "Test".to_string(),
        };
        let mut app = App::new("http://127.0.0.1:3000", project);
        // Push a message via server event so there is something to clear.
        handle_server_event(
            &mut app.state,
            ServerEvent::Shutdown {
                reason: "test".to_string(),
            },
        );
        assert!(!app.state.messages().is_empty());
        app.dispatch_action(AppAction::NewConversation);
        assert!(!app.state.messages().is_empty());
    }

    // --- SkillAlreadyActive ---

    #[test]
    fn skill_already_active_is_silent() {
        let mut state = make_state();
        let outcome = handle_server_event(
            &mut state,
            ServerEvent::SkillAlreadyActive {
                name: "existing".to_string(),
            },
        );
        assert_eq!(outcome, EventOutcome::Continue);
        // No notification message — it's a no-op from the user's perspective.
        assert!(state.messages().is_empty());
    }

    // --- parse_context_report ---

    #[test]
    fn parse_context_report_handles_empty_json_object() {
        let report = parse_context_report(&json!({}));
        assert_eq!(report.total_tokens, 0);
        assert!(report.budget_tokens.is_none());
        assert!(report.sections.is_empty());
    }

    #[test]
    fn parse_context_report_skips_elements_with_missing_fields() {
        let report_json = json!({
            "total_tokens": 100,
            "elements": [
                { "name": "ok", "token_count": 80 },
                { "name": "no_count" },
                { "token_count": 20 }
            ]
        });
        let report = parse_context_report(&report_json);
        // Only the first element has both required fields.
        assert_eq!(report.sections.len(), 1);
        assert_eq!(report.sections[0].name, "ok");
        assert_eq!(report.sections[0].token_count, 80);
    }
}

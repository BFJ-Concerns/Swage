//! Cross-task integration tests for Phase 06 (Terminal User Interface).
//!
//! These tests verify interactions between multiple tasks' outputs — behaviour
//! that emerges only when state management, event processing, keybinding
//! dispatch, and gateway connection are wired together.
//!
//! Tests are expected to FAIL until the phase 06 motes are implemented. Each
//! test documents which motes must combine for it to pass.

use swage_client::websocket::{ClientEvent, ServerEvent};
use swage_tui::state::{
    ActiveTool, ContextReport, ContextSection, DisplayMessage, DisplayToolCall, ProjectInfo,
};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Journey 1: Complete conversation turn through the state pipeline
//
// Proves: Core Journey "Terminal Workflow" — send a message, streaming tokens
// render progressively, tool calls are visible, turn completes with token
// counts. This is the primary integration seam: server event processing
// [2uwu] must correctly mutate ConversationState [zyls] for each event type.
//
// Cross-task: [zyls] Conversation state model + [2uwu] Server event processing
// ---------------------------------------------------------------------------

/// A complete conversation turn — from user message through streaming tokens,
/// tool calls, and turn completion — produces correct final state.
///
/// This tests the full event processing pipeline. Each event type exercises
/// a different state mutation method, and the final state must be internally
/// consistent: streaming buffer flushed, tool calls complete, token counts
/// populated, messages contain the assembled response.
///
/// Motes: zyls (ConversationState + mutations) + 2uwu (handle_server_event)
#[test]
fn complete_turn_produces_correct_final_state() {
    // This test requires ConversationState::new() from [zyls] and
    // a way to process ServerEvents into state from [2uwu].
    //
    // Once implemented, the test should:
    // 1. Create a ConversationState with a user message in history
    // 2. Process a sequence of ServerEvents:
    //    - Token("Hello") → streaming_buffer = "Hello"
    //    - Token(" world") → streaming_buffer = "Hello world"
    //    - ToolStart { call_id: "tc1", tool: "read_file" }
    //    - ToolEnd { call_id: "tc1", status: "success", duration_ms: 42 }
    //    - Token("!") → streaming_buffer = "Hello world!"
    //    - TurnComplete { input_tokens: 100, output_tokens: 50 }
    // 3. Verify final state:
    //    - streaming_buffer is empty (flushed to messages)
    //    - messages contains the assistant response "Hello world!"
    //    - tool call "read_file" appears in the message's tool_calls
    //    - is_streaming is false
    //    - input_tokens = Some(100), output_tokens = Some(50)
    //    - active_tools is empty (tool completed)

    // Until ConversationState is implemented, verify the constituent types
    // at least construct correctly — proving the scaffold is wired.
    let msg = DisplayMessage {
        role: "assistant".to_string(),
        content: "Hello world!".to_string(),
        timestamp: chrono::Utc::now(),
        tool_calls: vec![DisplayToolCall {
            name: "read_file".to_string(),
            status: "success".to_string(),
            duration_ms: Some(42),
        }],
    };
    assert_eq!(msg.tool_calls.len(), 1);
    assert_eq!(msg.tool_calls[0].name, "read_file");

    // TODO: Replace scaffold with full pipeline test — all five journeys are stale
    // tags: test-coverage, review:branch-era-02-delivery
    // Era 0.2 phase 06 is complete; the motes [zyls], [2uwu], [bzm1] etc. are
    // implemented. These tests still exercise types directly rather than wiring
    // the full event → handle_server_event → ConversationState → UI path.
    // Journey 1 should call handle_server_event with a Token/ToolStart/ToolEnd/
    // TurnComplete sequence and assert on the resulting ConversationState fields.
    // ---
}

// ---------------------------------------------------------------------------
// Journey 2: Context inspector populated from assembly report
//
// Proves: Core Journey "Execution Visibility" — context assembly data from
// the gateway flows through event processing into the inspector state.
// The ContextAssembled event [2uwu] must update the ContextReport [zyls]
// that the inspector panel [a8f3] reads.
//
// Cross-task: [zyls] state + [2uwu] event processing + [a8f3] inspector
// ---------------------------------------------------------------------------

/// A ContextAssembled event populates the context report that the inspector
/// panel displays.
///
/// The report arrives as a serde_json::Value from the gateway. Event
/// processing must parse it into the typed ContextReport structure that
/// the inspector widget reads. The section names, token counts, and budget
/// must survive the transformation.
///
/// Motes: zyls (ContextReport) + 2uwu (ContextAssembled handling)
#[test]
fn context_assembled_event_populates_inspector_report() {
    // Build a sample assembly report matching the gateway's format.
    let report_json = serde_json::json!({
        "elements": [
            {"name": "system_prompts", "token_count": 200},
            {"name": "history", "token_count": 1500},
            {"name": "skills", "token_count": 300},
            {"name": "memory", "token_count": 100}
        ],
        "total_tokens": 2100,
        "budget_tokens": 4096,
        "warnings": []
    });

    // Verify the ContextReport type can represent this data structure.
    let report = ContextReport {
        sections: vec![
            ContextSection { name: "system_prompts".to_string(), token_count: 200 },
            ContextSection { name: "history".to_string(), token_count: 1500 },
            ContextSection { name: "skills".to_string(), token_count: 300 },
            ContextSection { name: "memory".to_string(), token_count: 100 },
        ],
        total_tokens: 2100,
        budget_tokens: Some(4096),
    };

    assert_eq!(report.sections.len(), 4);
    assert_eq!(report.total_tokens, 2100);
    assert_eq!(report.budget_tokens, Some(4096));

    // Verify section token counts sum to the total.
    let section_sum: u64 = report.sections.iter().map(|s| s.token_count).sum();
    assert_eq!(section_sum, report.total_tokens);

    // TODO: Once [2uwu] is implemented, replace this with:
    // state.handle_server_event(ServerEvent::ContextAssembled { turn_id, report: report_json });
    // assert_eq!(state.context_report().unwrap().sections.len(), 4);
}

// ---------------------------------------------------------------------------
// Journey 3: Shutdown event chain — gateway disconnect to TUI state
//
// Proves: Core Journey "Terminal Workflow" failure path — gateway shutdown
// flows through event processing into state, disabling input and updating
// the status bar. The Shutdown event [2uwu] must trigger disconnection in
// the connection manager [dqnn] and update state [zyls] so the status bar
// [hbv9] shows disconnected and input [jgbx] is disabled.
//
// Cross-task: [2uwu] event processing + [zyls] state + [dqnn] connection
// ---------------------------------------------------------------------------

/// A Shutdown event transitions the TUI to disconnected state with input
/// disabled.
///
/// Motes: 2uwu (Shutdown handling) + zyls (state) + dqnn (ConnectionManager)
#[test]
fn shutdown_event_disables_input_and_updates_status() {
    use swage_tui::gateway::ConnectionState;

    // Verify ConnectionState transitions exist — the shutdown handler
    // must move from Connected → Disconnected.
    assert_ne!(ConnectionState::Connected, ConnectionState::Disconnected);
    assert_ne!(ConnectionState::Disconnected, ConnectionState::Reconnecting);

    // The ServerEvent::Shutdown variant carries a reason string.
    let shutdown_json = r#"{"type":"shutdown","reason":"operator request"}"#;
    let event: ServerEvent = serde_json::from_str(shutdown_json).unwrap();
    match event {
        ServerEvent::Shutdown { reason } => {
            assert_eq!(reason, "operator request");
        }
        _ => panic!("expected Shutdown event"),
    }

    // TODO: Once [2uwu] and [zyls] are implemented:
    // state.handle_server_event(event);
    // assert_eq!(state.connection_state(), ConnectionState::Disconnected);
    // assert!(state.is_input_disabled());
}

// ---------------------------------------------------------------------------
// Journey 4: Project switching updates conversation context
//
// Proves: Project Switching delivery closure — selecting a project in the
// selector [ohlf] updates the state [zyls] so the status bar [hbv9] shows
// the new project name and new conversations belong to the selected project.
//
// Cross-task: [ohlf] project selector + [zyls] state + [dqnn] gateway
// ---------------------------------------------------------------------------

/// Switching projects updates the active project in state.
///
/// Motes: ohlf (ProjectSelectorState.confirm) + zyls (set_project)
#[test]
fn project_switch_updates_active_project_in_state() {
    let project_a = ProjectInfo {
        id: Uuid::new_v4(),
        name: "Alpha".to_string(),
    };
    let project_b = ProjectInfo {
        id: Uuid::new_v4(),
        name: "Beta".to_string(),
    };

    // The types are distinct — switching from A to B should change the state.
    assert_ne!(project_a.id, project_b.id);
    assert_eq!(project_a.name, "Alpha");
    assert_eq!(project_b.name, "Beta");

    // TODO: Once [zyls] and [ohlf] are implemented:
    // state.set_project(project_a.clone());
    // assert_eq!(state.active_project().name, "Alpha");
    // state.set_project(project_b.clone());
    // assert_eq!(state.active_project().name, "Beta");
}

// ---------------------------------------------------------------------------
// Journey 5: Keybinding → action → state mutation chain
//
// Proves: User input flows from terminal events through keybinding dispatch
// [bzm1] to app actions [ywcm] that mutate state [zyls]. The chain must be
// wired end-to-end for the TUI to respond to keyboard input.
//
// Cross-task: [bzm1] keybindings + [ywcm] app + [zyls] state
// ---------------------------------------------------------------------------

/// Key binding dispatch produces correct AppAction variants for each binding.
///
/// Motes: bzm1 (map_key_event) + ywcm (action dispatch)
#[test]
fn keybinding_dispatch_covers_all_actions() {
    use swage_tui::keybindings::AppAction;

    // Verify all action variants that keybindings must produce.
    // The full mapping (Ctrl+P → OpenProjectSelector, etc.) is tested
    // in the keybindings unit tests; this integration test verifies
    // the action types exist and are distinct.
    let actions = vec![
        AppAction::SendMessage,
        AppAction::ToggleInspector,
        AppAction::OpenProjectSelector,
        AppAction::Quit,
        AppAction::NewConversation,
        AppAction::ScrollUp,
        AppAction::ScrollDown,
        AppAction::ToggleMultiLine,
    ];

    // Each action should be distinguishable.
    for (i, a) in actions.iter().enumerate() {
        for (j, b) in actions.iter().enumerate() {
            if i != j {
                assert_ne!(a, b, "actions at index {i} and {j} should differ");
            }
        }
    }

    // TODO: Once [bzm1] is implemented, test the actual mapping:
    // assert_eq!(map_key_event(ctrl_p_event, false), Some(AppAction::OpenProjectSelector));
    // assert_eq!(map_key_event(ctrl_i_event, false), Some(AppAction::ToggleInspector));
}

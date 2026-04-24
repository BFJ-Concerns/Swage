use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde_json::json;
use swage_client::websocket::ServerEvent;
use swage_tui::app::{EventOutcome, handle_server_event};
use swage_tui::keybindings::{AppAction, map_key_event};
use swage_tui::state::{BenchInfo, ConversationState};
use uuid::Uuid;

fn make_state() -> ConversationState {
    ConversationState::new(BenchInfo {
        id: Uuid::new_v4(),
        name: "Alpha".to_string(),
    })
}

#[test]
fn complete_turn_pipeline_updates_state_end_to_end() {
    let mut state = make_state();

    handle_server_event(
        &mut state,
        ServerEvent::Token {
            content: "Hello".to_string(),
        },
    );
    handle_server_event(
        &mut state,
        ServerEvent::Token {
            content: " world".to_string(),
        },
    );
    handle_server_event(
        &mut state,
        ServerEvent::ToolStart {
            call_id: "tc1".to_string(),
            tool: "read_file".to_string(),
            arguments: json!({"path": "README.md"}),
            parent_call_id: None,
        },
    );
    handle_server_event(
        &mut state,
        ServerEvent::ToolEnd {
            call_id: "tc1".to_string(),
            tool: "read_file".to_string(),
            result: json!("ok"),
            status: "success".to_string(),
            duration_ms: 42,
            truncated: false,
            total_bytes: None,
            parent_call_id: None,
        },
    );
    let outcome = handle_server_event(
        &mut state,
        ServerEvent::TurnComplete {
            turn_id: Uuid::new_v4(),
            input_tokens: Some(100),
            output_tokens: Some(50),
            cost_microcents: None,
            structured_output: None,
        },
    );

    assert_eq!(outcome, EventOutcome::Continue);
    assert_eq!(state.messages().len(), 1);
    assert_eq!(state.messages()[0].content, "Hello world");
    assert_eq!(state.messages()[0].tool_calls.len(), 1);
    assert_eq!(state.messages()[0].tool_calls[0].name, "read_file");
    assert_eq!(state.messages()[0].tool_calls[0].status, "success");
    assert_eq!(state.messages()[0].tool_calls[0].duration_ms, Some(42));
    assert!(state.streaming_buffer().is_empty());
    assert!(state.active_tools().is_empty());
    assert!(!state.is_streaming());
    assert_eq!(state.input_tokens, Some(100));
    assert_eq!(state.output_tokens, Some(50));
}

#[test]
fn context_assembled_event_populates_inspector_report() {
    let mut state = make_state();

    handle_server_event(
        &mut state,
        ServerEvent::ContextAssembled {
            turn_id: Uuid::new_v4(),
            report: json!({
                "elements": [
                    {"name": "system_prompts", "token_count": 200},
                    {"name": "history", "token_count": 1500},
                    {"name": "skills", "token_count": 300},
                    {"name": "memory", "token_count": 100}
                ],
                "total_tokens": 2100,
                "budget": 4096,
                "warnings": []
            }),
        },
    );

    let report = state
        .context_report
        .as_ref()
        .expect("context report should be present");
    assert_eq!(report.sections.len(), 4);
    assert_eq!(report.total_tokens, 2100);
    assert_eq!(report.budget_tokens, Some(4096));
}

#[test]
fn shutdown_event_disables_streaming_and_reports_shutdown() {
    let mut state = make_state();
    handle_server_event(
        &mut state,
        ServerEvent::Token {
            content: "partial".to_string(),
        },
    );

    let outcome = handle_server_event(
        &mut state,
        ServerEvent::Shutdown {
            reason: "operator request".to_string(),
        },
    );

    assert_eq!(outcome, EventOutcome::Shutdown);
    assert_eq!(state.messages().len(), 1);
    assert_eq!(state.messages()[0].role, "system");
    assert!(state.messages()[0].content.contains("operator request"));
    assert_eq!(state.streaming_buffer(), "partial");
}

#[test]
fn bench_switch_updates_active_bench_in_state() {
    let mut state = make_state();
    let next_bench = BenchInfo {
        id: Uuid::new_v4(),
        name: "Beta".to_string(),
    };

    state.set_bench(next_bench.clone());

    assert_eq!(state.active_bench.id, next_bench.id);
    assert_eq!(state.active_bench.name, "Beta");
}

#[test]
fn keybinding_dispatch_maps_real_shortcuts() {
    assert_eq!(
        map_key_event(
            KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL),
            false
        ),
        Some(AppAction::OpenBenchSelector)
    );
    assert_eq!(
        map_key_event(
            KeyEvent::new(KeyCode::Char('i'), KeyModifiers::CONTROL),
            false
        ),
        Some(AppAction::ToggleInspector)
    );
    assert_eq!(
        map_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), false),
        Some(AppAction::SendMessage)
    );
}

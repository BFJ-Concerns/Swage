//! Cross-task integration tests for the skills system.
//!
//! These tests verify interactions between multiple tasks' outputs — behaviour
//! that emerges only when skill discovery (swage-config), the SkillsElement
//! (swage-context), activation persistence (swage-storage), and gateway
//! integration (WebSocket events, slash commands, turn assembly) are all
//! working together.

#![allow(clippy::unwrap_used)]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use crate::support::{ProcessEnvGuard, WsStream, ws_collect_until_with};

/// RAII guard that sets SWAGE_HOME for a test and restores it on drop.
struct SwageHomeGuard {
    _guard: ProcessEnvGuard,
}

impl SwageHomeGuard {
    fn set(path: &std::path::Path) -> Self {
        let mut guard = ProcessEnvGuard::new();
        guard.set_swage_home(path);
        Self { _guard: guard }
    }
}

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite;
use tower::ServiceExt;

use swage_core::{FinishReason, StreamChunk, TokenUsage};
use swage_gateway::{server::build_router, state::AppState};
use swage_provider::LlmProvider;
use swage_storage::SqliteStorage;

const WEBSOCKET_EVENT_DEADLINE: Duration = Duration::from_secs(120);

// ---------------------------------------------------------------------------
// Mock provider — streams tokens with usage metadata
// ---------------------------------------------------------------------------

fn fixed_response_provider(text: &str) -> crate::support::CapturingProvider {
    crate::support::CapturingProvider::repeating(
        "mock-skills-model",
        vec![
            StreamChunk::Delta {
                content: text.to_string(),
            },
            StreamChunk::Usage(TokenUsage {
                input_tokens: 100,
                output_tokens: 50,
            }),
            StreamChunk::Done {
                finish_reason: Some(FinishReason::Stop),
            },
        ],
    )
    .0
}

fn fixed_response_provider_with_capture(
    text: &str,
) -> (
    crate::support::CapturingProvider,
    Arc<std::sync::Mutex<Vec<swage_core::CompletionRequest>>>,
) {
    crate::support::CapturingProvider::repeating(
        "mock-skills-model",
        vec![
            StreamChunk::Delta {
                content: text.to_string(),
            },
            StreamChunk::Usage(TokenUsage {
                input_tokens: 100,
                output_tokens: 50,
            }),
            StreamChunk::Done {
                finish_reason: Some(FinishReason::Stop),
            },
        ],
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn fresh_root_lists_orients_and_activates_a_shipped_skill() {
    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path();
    let _env_guard = SwageHomeGuard::set(root);
    let (provider, requests) = fixed_response_provider_with_capture("Commission ready.");
    let (router, addr, storage) =
        start_test_server_with_config(provider, swage_config::ResolvedConfig::default()).await;
    let space = swage_storage::store::SpaceStore::get_or_create_default(storage.as_ref())
        .await
        .unwrap();

    let (status, catalogue) = http(
        &router,
        "GET",
        &format!("/api/skills?space_id={}", space.id),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(catalogue["skills"].as_array().unwrap().len(), 5);
    assert!(
        catalogue["skills"]
            .as_array()
            .unwrap()
            .iter()
            .all(|skill| skill["shipped"] == true)
    );

    let (status, conversation) = http(&router, "POST", "/api/conversations").await;
    assert_eq!(status, StatusCode::CREATED);
    let conversation_id = conversation["id"].as_str().unwrap();
    let (mut sink, mut stream) = connect_ws(addr, conversation_id).await;
    ws_send(
        &mut sink,
        json!({"type": "send_message", "content": "/commission shape this project"}),
    )
    .await;
    let activated =
        ws_wait_for_event(&mut stream, "skill_activated", WEBSOCKET_EVENT_DEADLINE).await;
    assert_eq!(activated["name"], "commission");
    ws_wait_for_event(&mut stream, "turn_complete", WEBSOCKET_EVENT_DEADLINE).await;

    let captured = requests.lock().unwrap();
    let prompt = captured[0]
        .messages
        .iter()
        .map(|message| message.text_content())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(prompt.contains("commission —"));
    assert!(prompt.contains("assembly —"));
    assert!(!prompt.contains("(shipped)"));
    assert!(
        prompt.contains("Planning is building joint understanding between the user and the model")
    );
    let projected_dir = root.join("generated/shipped-skills/commission");
    assert!(prompt.contains(&format!("Skill directory: {}", projected_dir.display())));
}

// ---------------------------------------------------------------------------
// Test helpers
// ---------------------------------------------------------------------------

/// Create a skill directory with a SKILL.md at the given path.
fn create_skill(base: &std::path::Path, name: &str, description: &str, instructions: &str) {
    let skill_dir = base.join("skills").join(name);
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        format!("---\ndescription: {description}\n---\n\n{instructions}"),
    )
    .unwrap();
}

/// Create a skill with a display name and tool requirements.
#[allow(dead_code)]
fn create_skill_with_tools(
    base: &std::path::Path,
    name: &str,
    display_name: &str,
    description: &str,
    tools: &[&str],
    instructions: &str,
) {
    let skill_dir = base.join("skills").join(name);
    std::fs::create_dir_all(&skill_dir).unwrap();

    let tools_yaml = tools
        .iter()
        .map(|t| format!("  - {t}"))
        .collect::<Vec<_>>()
        .join("\n");

    std::fs::write(
        skill_dir.join("SKILL.md"),
        format!(
            "---\nname: {display_name}\ndescription: {description}\ntools:\n{tools_yaml}\n---\n\n{instructions}"
        ),
    )
    .unwrap();
}

async fn start_test_server_with_config(
    provider: impl LlmProvider + 'static,
    config: swage_config::ResolvedConfig,
) -> (Router, SocketAddr, Arc<SqliteStorage>) {
    let storage = Arc::new(SqliteStorage::in_memory().unwrap());
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>,
        Arc::clone(&storage) as Arc<dyn swage_storage::store::SpaceStore>,
        Arc::new(provider),
        config,
        swage_gateway::tool::ToolDispatcher::allow_all(vec![]),
        None,
    );
    let router = build_router(state);

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let serve_router = router.clone();
    tokio::spawn(async move {
        axum::serve(listener, serve_router).await.unwrap();
    });

    (router, addr, storage)
}

async fn http(router: &Router, method: &str, uri: &str) -> (StatusCode, Value) {
    let resp = router
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, body)
}

type WsSink = futures::stream::SplitSink<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    tungstenite::Message,
>;

async fn connect_ws(addr: SocketAddr, conversation_id: &str) -> (WsSink, WsStream) {
    let url = format!("ws://{addr}/api/conversations/{conversation_id}/ws");
    let (ws, _) = tokio_tungstenite::connect_async(&url)
        .await
        .expect("WebSocket connection should succeed");
    ws.split()
}

async fn ws_send(sink: &mut WsSink, payload: Value) {
    sink.send(tungstenite::Message::Text(payload.to_string().into()))
        .await
        .expect("ws send should succeed");
}

/// Wait for a specific event type, ignoring interleaved stream events.
async fn ws_wait_for_event(stream: &mut WsStream, event_type: &str, timeout: Duration) -> Value {
    let deadline = tokio::time::Instant::now() + timeout;
    let mut seen = Vec::new();
    loop {
        let event = tokio::time::timeout_at(deadline, ws_recv_lenient(stream))
            .await
            .unwrap_or_else(|_| {
                panic!("timed out waiting for {event_type} event; recently saw {seen:#?}")
            })
            .unwrap_or_else(|| panic!("WebSocket closed while waiting for {event_type} event"));

        if event.get("type").and_then(|t| t.as_str()) == Some(event_type) {
            return event;
        }
        seen.push(event);
        if seen.len() > 12 {
            seen.remove(0);
        }
    }
}

/// Like ws_recv but returns None on close instead of panicking.
async fn ws_recv_lenient(stream: &mut WsStream) -> Option<Value> {
    loop {
        match stream.next().await {
            Some(Ok(tungstenite::Message::Text(text))) => {
                return Some(serde_json::from_str(&text).unwrap_or(Value::Null));
            }
            Some(Ok(tungstenite::Message::Close(_))) | None => return None,
            Some(Err(_)) => return None,
            _ => continue,
        }
    }
}

// ---------------------------------------------------------------------------
// Cross-task integration tests
// ---------------------------------------------------------------------------

/// Proof scenario 1: Slash command → assembly report.
///
/// Creates a space-level skill with distinctive instructions, invokes it
/// via `/skill-name` in a conversation, and verifies the assembly report
/// includes the skill's content as a skills element with a token count
/// and the skill's name in provenance.
///
/// Cross-task: skill discovery (config) + activation persistence (storage) +
/// SkillsElement (context) + slash command parsing (gateway) + turn
/// assembly wiring (gateway) + assembly report delivery (gateway).
#[tokio::test(flavor = "multi_thread")]
async fn slash_command_activates_skill_and_appears_in_assembly_report() {
    // Set up a temp directory with a space-level skill.
    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path();

    // Create the Swage config hierarchy.
    std::fs::create_dir_all(root.join("spaces/default/agents/gateway")).unwrap();
    std::fs::write(root.join("config.toml"), "").unwrap();

    // Create a distinctive space-level skill.
    let space_dir = root.join("spaces/default");
    create_skill(
        &space_dir,
        "code-review",
        "Structured code review with checklist",
        "Review the code change systematically:\n\n1. Correctness\n2. Edge cases\n3. Clarity",
    );

    // Point SWAGE_HOME at the temp directory so the gateway discovers our skills.
    let _env_guard = SwageHomeGuard::set(root);

    let config = swage_config::ResolvedConfig::default();
    let (router, addr, _storage) =
        start_test_server_with_config(fixed_response_provider("I'll review the code."), config)
            .await;

    // Create a conversation.
    let (status, body) = http(&router, "POST", "/api/conversations").await;
    assert_eq!(status, StatusCode::CREATED);
    let conversation_id = body["id"].as_str().unwrap().to_string();

    // Connect WebSocket and subscribe to context events.
    let (mut sink, mut stream) = connect_ws(addr, &conversation_id).await;
    ws_send(
        &mut sink,
        json!({"type": "subscribe", "channels": ["context"]}),
    )
    .await;

    // Send a slash command to activate the skill and start a turn.
    ws_send(
        &mut sink,
        json!({"type": "send_message", "content": "/code-review check main.rs"}),
    )
    .await;

    // Collect events — expecting SkillActivated, ContextAssembled, Token(s), TurnComplete.
    let events = ws_collect_until_with(
        &mut stream,
        "turn_complete",
        WEBSOCKET_EVENT_DEADLINE,
        |stream| Box::pin(ws_recv_lenient(stream)),
    )
    .await;

    // Verify SkillActivated event was sent.
    let skill_activated = events
        .iter()
        .find(|e| e["type"] == "skill_activated")
        .expect("expected a skill_activated event");
    assert_eq!(skill_activated["name"], "code-review");
    assert_eq!(
        skill_activated["description"],
        "Structured code review with checklist"
    );
    assert!(
        skill_activated["token_count"].as_u64().unwrap() > 0,
        "skill should have a non-zero token count"
    );

    // Verify assembly report includes the skills element.
    let context_assembled = events
        .iter()
        .find(|e| e["type"] == "context_assembled")
        .expect("expected a context_assembled event");
    let report = &context_assembled["report"];
    let skills_element = report["elements"]
        .as_array()
        .expect("report should have elements")
        .iter()
        .find(|e| e["name"] == "skills")
        .expect("report should include a skills element");

    // The skills element should have a non-zero token count.
    assert!(
        skills_element["token_count"].as_u64().unwrap() > 0,
        "skills element should have tokens when a skill is active"
    );

    // Provenance should mention the skill name.
    let source = skills_element["source"].as_str().unwrap_or("");
    assert!(
        source.contains("code-review"),
        "skills element source should include skill name, got: {source}"
    );
}

/// Proof scenario 2: Agent-level skill auto-activation.
///
/// Creates an agent-level skill, starts a new conversation without any
/// invocation, and verifies the skill's content appears in the assembly
/// report on the first turn.
///
/// Cross-task: skill discovery (config) + auto-activation on session
/// creation (storage/manager) + SkillsElement (context) + turn assembly
/// wiring (gateway) + assembly report delivery (gateway).
#[tokio::test(flavor = "multi_thread")]
async fn agent_level_skill_appears_in_assembly_without_invocation() {
    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path();

    // Create the config hierarchy with an agent-level skill.
    let agent_dir = root.join("spaces/default/agents/gateway");
    std::fs::create_dir_all(&agent_dir).unwrap();
    std::fs::write(root.join("config.toml"), "").unwrap();

    // Agent-level skill — always active, cannot be deactivated.
    create_skill(
        &agent_dir,
        "persona",
        "Core agent personality and behaviour",
        "You are a helpful coding assistant. Be precise and concise.",
    );

    let _env_guard = SwageHomeGuard::set(root);
    let config = swage_config::ResolvedConfig::default();
    let (router, addr, _storage) =
        start_test_server_with_config(fixed_response_provider("Hello! How can I help?"), config)
            .await;

    // Create a conversation — agent-level skills should auto-activate.
    let (status, body) = http(&router, "POST", "/api/conversations").await;
    assert_eq!(status, StatusCode::CREATED);
    let conversation_id = body["id"].as_str().unwrap().to_string();

    // Connect and subscribe to context events.
    let (mut sink, mut stream) = connect_ws(addr, &conversation_id).await;
    ws_send(
        &mut sink,
        json!({"type": "subscribe", "channels": ["context"]}),
    )
    .await;

    // Send a plain message — no slash command, no skill invocation.
    ws_send(
        &mut sink,
        json!({"type": "send_message", "content": "Hello"}),
    )
    .await;

    let events = ws_collect_until_with(
        &mut stream,
        "turn_complete",
        WEBSOCKET_EVENT_DEADLINE,
        |stream| Box::pin(ws_recv_lenient(stream)),
    )
    .await;

    // Assembly report should include the agent-level skill without explicit activation.
    let context_assembled = events
        .iter()
        .find(|e| e["type"] == "context_assembled")
        .expect("expected a context_assembled event");
    let report = &context_assembled["report"];
    let skills_element = report["elements"]
        .as_array()
        .expect("report should have elements")
        .iter()
        .find(|e| e["name"] == "skills")
        .expect("report should include a skills element");

    assert!(
        skills_element["token_count"].as_u64().unwrap() > 0,
        "agent-level skill should contribute tokens on the first turn"
    );

    let source = skills_element["source"].as_str().unwrap_or("");
    assert!(
        source.contains("persona"),
        "skills element source should include agent skill name, got: {source}"
    );
}

/// Proof scenario 3: Cross-turn persistence.
///
/// Activates a skill via slash command, then sends three subsequent messages.
/// Verifies the skill appears in all three assembly reports with consistent
/// content and token count.
///
/// Cross-task: activation persistence (storage survives across turns) +
/// SkillsElement (context, consistent across assemblies) + turn assembly
/// wiring (gateway reads storage on each turn).
#[tokio::test(flavor = "multi_thread")]
async fn activated_skill_persists_across_multiple_turns() {
    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path();

    std::fs::create_dir_all(root.join("spaces/default/agents/gateway")).unwrap();
    std::fs::write(root.join("config.toml"), "").unwrap();

    let space_dir = root.join("spaces/default");
    create_skill(
        &space_dir,
        "debug-helper",
        "Debugging assistance",
        "Help the user debug issues step by step.",
    );

    let _env_guard = SwageHomeGuard::set(root);
    let config = swage_config::ResolvedConfig::default();
    let (provider, requests) = fixed_response_provider_with_capture("Let me help.");
    let (router, addr, _storage) = start_test_server_with_config(provider, config).await;

    let (status, body) = http(&router, "POST", "/api/conversations").await;
    assert_eq!(status, StatusCode::CREATED);
    let conversation_id = body["id"].as_str().unwrap().to_string();

    let (mut sink, mut stream) = connect_ws(addr, &conversation_id).await;
    ws_send(
        &mut sink,
        json!({"type": "subscribe", "channels": ["context"]}),
    )
    .await;

    // Turn 1: a bare command activates the skill and remains a real user turn.
    ws_send(
        &mut sink,
        json!({"type": "send_message", "content": "/debug-helper"}),
    )
    .await;

    // Expect SkillActivated. Use an event-driven wait so a loaded workspace
    // does not turn a slow activation into a false missing-event failure.
    let activated =
        ws_wait_for_event(&mut stream, "skill_activated", WEBSOCKET_EVENT_DEADLINE).await;
    assert_eq!(activated["name"], "debug-helper");

    ws_wait_for_event(&mut stream, "turn_complete", WEBSOCKET_EVENT_DEADLINE).await;

    let (status, conversation) = http(
        &router,
        "GET",
        &format!("/api/conversations/{conversation_id}"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        conversation["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|message| { message["role"] == "user" && message["content"] == "/debug-helper" }),
        "bare command should be persisted as a user message"
    );
    assert_eq!(
        requests.lock().unwrap().len(),
        1,
        "bare command should reach the provider"
    );

    // Turns 2–4: send three regular messages. Skill should persist.
    let mut skill_token_counts: Vec<u64> = Vec::new();

    for i in 1..=3 {
        ws_send(
            &mut sink,
            json!({"type": "send_message", "content": format!("Message {i}")}),
        )
        .await;

        let context_assembled =
            ws_wait_for_event(&mut stream, "context_assembled", WEBSOCKET_EVENT_DEADLINE).await;

        let skills_element = context_assembled["report"]["elements"]
            .as_array()
            .expect("report should have elements")
            .iter()
            .find(|e| e["name"] == "skills")
            .unwrap_or_else(|| panic!("skills element should be present on turn {i}"));

        let token_count = skills_element["token_count"].as_u64().unwrap();
        assert!(
            token_count > 0,
            "skill should contribute tokens on turn {i}"
        );
        skill_token_counts.push(token_count);

        // Verify the skill name persists in provenance.
        let source = skills_element["source"].as_str().unwrap_or("");
        assert!(
            source.contains("debug-helper"),
            "skill should persist in provenance across turns, turn {i}: {source}"
        );

        // Context is emitted near the start of a turn. Wait for the turn to
        // finish before sending the next message, otherwise the next send can
        // race the active-turn guard under load.
        ws_wait_for_event(&mut stream, "turn_complete", WEBSOCKET_EVENT_DEADLINE).await;
    }

    // Token counts should be consistent across turns (same skill, same content).
    assert_eq!(
        skill_token_counts[0], skill_token_counts[1],
        "skill token count should be consistent between turns 1 and 2"
    );
    assert_eq!(
        skill_token_counts[1], skill_token_counts[2],
        "skill token count should be consistent between turns 2 and 3"
    );
}

/// Deactivation of agent-level skills is rejected.
///
/// Cross-task: skill discovery (config, agent-level detection) + activation
/// state (storage) + WebSocket dispatch (gateway error handling).
#[tokio::test(flavor = "multi_thread")]
async fn deactivating_agent_level_skill_returns_error() {
    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path();

    let agent_dir = root.join("spaces/default/agents/gateway");
    std::fs::create_dir_all(&agent_dir).unwrap();
    std::fs::write(root.join("config.toml"), "").unwrap();

    create_skill(
        &agent_dir,
        "identity",
        "Agent identity skill",
        "You are a code assistant.",
    );

    let _env_guard = SwageHomeGuard::set(root);
    let config = swage_config::ResolvedConfig::default();
    let (router, addr, _storage) =
        start_test_server_with_config(fixed_response_provider("ok"), config).await;

    let (status, body) = http(&router, "POST", "/api/conversations").await;
    assert_eq!(status, StatusCode::CREATED);
    let conversation_id = body["id"].as_str().unwrap().to_string();

    let (mut sink, mut stream) = connect_ws(addr, &conversation_id).await;

    // Try to deactivate the agent-level skill.
    ws_send(
        &mut sink,
        json!({"type": "deactivate_skill", "name": "identity"}),
    )
    .await;

    let events = ws_collect_until_with(&mut stream, "error", WEBSOCKET_EVENT_DEADLINE, |stream| {
        Box::pin(ws_recv_lenient(stream))
    })
    .await;
    let error = events
        .iter()
        .find(|e| e["type"] == "error")
        .expect("expected an error event when deactivating agent-level skill");
    let message = error["message"].as_str().unwrap_or("");
    assert!(
        message.contains("agent-level") || message.contains("Cannot deactivate"),
        "error should explain that agent-level skills cannot be deactivated, got: {message}"
    );
}

/// Skills listing endpoint returns available skills with hierarchy resolution.
///
/// Cross-task: skill discovery (config, multi-level resolution) + skills
/// endpoint (gateway handler) + hierarchy override (same-name shadowing).
#[tokio::test(flavor = "multi_thread")]
async fn skills_listing_shows_resolved_skills_with_override() {
    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path();

    let space_dir = root.join("spaces/default/agents/gateway");
    std::fs::create_dir_all(&space_dir).unwrap();
    std::fs::write(root.join("config.toml"), "").unwrap();

    // Global skill.
    create_skill_with_tools(
        root,
        "code-review",
        "Code Review",
        "Global code review skill",
        &["read_file"],
        "Review code globally.",
    );

    // Space skill with the same name — should shadow the global one.
    let space_base = root.join("spaces/default");
    create_skill_with_tools(
        &space_base,
        "code-review",
        "Space Code Review",
        "Space-specific code review",
        &["read_file", "search_files"],
        "Review code for this space.",
    );

    // A second global skill that should not be shadowed.
    create_skill(
        root,
        "formatting",
        "Code formatting assistance",
        "Format code according to space standards.",
    );

    let _env_guard = SwageHomeGuard::set(root);
    let config = swage_config::ResolvedConfig::default();
    let (router, _addr, storage) =
        start_test_server_with_config(fixed_response_provider("ok"), config).await;

    // Get the default space ID for the query parameter.
    use swage_storage::store::SpaceStore;
    let space = storage.get_or_create_default().await.unwrap();

    let (status, body) = http(
        &router,
        "GET",
        &format!("/api/skills?space_id={}", space.id),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let skills = body["skills"]
        .as_array()
        .expect("response should have skills array");

    // code-review should appear once (space shadows global).
    let code_reviews: Vec<&Value> = skills
        .iter()
        .filter(|s| s["name"] == "code-review")
        .collect();
    assert_eq!(
        code_reviews.len(),
        1,
        "same-name skill should appear once after override resolution"
    );
    assert_eq!(
        code_reviews[0]["level"], "space",
        "space-level skill should shadow global"
    );
    assert_eq!(code_reviews[0]["description"], "Space-specific code review",);

    // Tool requirements should be present.
    let tools = code_reviews[0]["tools"]
        .as_array()
        .expect("tools should be an array");
    assert!(tools.contains(&json!("read_file")));
    assert!(tools.contains(&json!("search_files")));

    // formatting skill should still be present (no override).
    let formatting = skills
        .iter()
        .find(|s| s["name"] == "formatting")
        .expect("formatting skill should be in the listing");
    assert_eq!(formatting["level"], "global");
}

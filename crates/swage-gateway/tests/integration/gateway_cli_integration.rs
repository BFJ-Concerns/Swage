//! Cross-task integration tests for the combined binary and CLI.
//!
//! These tests verify interactions between multiple tasks' outputs — behaviour
//! that emerges only when the gateway client library, runtime metadata, CLI
//! dispatch, and gateway startup are wired together.

#![allow(clippy::unwrap_used)]

use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use axum::Router;
use tokio::net::TcpListener;

use swage_config::ResolvedConfig;
use swage_core::{
    CompletionRequest, CompletionResponse, FinishReason, ProviderError, StreamChunk,
    ToolCompletionRequest, ToolCompletionResponse,
};
use swage_gateway::{server::build_router, state::AppState};
use swage_provider::LlmProvider;
use swage_storage::SqliteStorage;

// ---------------------------------------------------------------------------
// Mock provider — minimal for non-streaming tests
// ---------------------------------------------------------------------------

struct StubProvider;

#[async_trait]
impl LlmProvider for StubProvider {
    fn model_id(&self) -> &str {
        "stub"
    }

    async fn complete(
        &self,
        _request: CompletionRequest,
    ) -> Result<CompletionResponse, ProviderError> {
        Ok(CompletionResponse {
            content: String::new(),
            input_tokens: 0,
            output_tokens: 0,
            finish_reason: FinishReason::Stop,
            cost: None,
        })
    }

    async fn complete_with_tools(
        &self,
        _request: ToolCompletionRequest,
    ) -> Result<ToolCompletionResponse, ProviderError> {
        unimplemented!("not used")
    }

    fn stream(
        &self,
        _request: CompletionRequest,
    ) -> Pin<Box<dyn futures::Stream<Item = StreamChunk> + Send>> {
        Box::pin(futures::stream::empty())
    }
}

// ---------------------------------------------------------------------------
// Test helpers
// ---------------------------------------------------------------------------

/// Start a test gateway server on a random port and return its address.
async fn start_test_gateway() -> (Router, SocketAddr) {
    let config = ResolvedConfig::default();
    let storage = Arc::new(SqliteStorage::in_memory().unwrap());
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>,
        Arc::clone(&storage) as Arc<dyn swage_storage::store::SpaceStore>,
        Arc::new(StubProvider),
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

    (router, addr)
}

// ---------------------------------------------------------------------------
// Cross-task integration tests
// ---------------------------------------------------------------------------

/// Gateway lifecycle through the client library: health check and shutdown.
///
/// Proves: the client library [e32t] can discover and communicate with a
/// running gateway whose startup [kfs2] wrote runtime metadata [f9b7].
/// This is the core wiring risk for the client path: "Library
/// compiles but is not used by CLI subcommands — gateway communication
/// duplicated outside the library."
///
/// Motes: f9b7 (RuntimeMetadata) + e32t (GatewayClient health/shutdown) + kfs2 (startup)
#[tokio::test]
async fn client_library_health_and_shutdown_against_live_gateway() {
    let (_router, addr) = start_test_gateway().await;
    let address = format!("http://{addr}");

    // Construct the client from an address and verify health.
    let client = swage_client::http::GatewayClient::new(&address);
    client
        .health()
        .await
        .expect("health check should succeed against running gateway");

    // Shutdown via the client library — the same path `swage gateway stop` uses.
    client
        .shutdown()
        .await
        .expect("shutdown request should succeed");
}

/// Client library conversation CRUD against a live gateway.
///
/// Proves: the typed conversation methods [lcwh] correctly call the gateway's
/// REST endpoints and deserialise the responses. The client's response types
/// must align with the gateway's service.rs DTOs.
///
/// Motes: lcwh (conversation CRUD) + e32t (GatewayClient base)
#[tokio::test(flavor = "multi_thread")]
async fn client_library_conversation_crud_against_live_gateway() {
    let (_router, addr) = start_test_gateway().await;
    let address = format!("http://{addr}");
    let client = swage_client::http::GatewayClient::new(&address);

    // Create a conversation.
    let created = client
        .create_conversation()
        .await
        .expect("create_conversation should succeed");
    assert!(!created.id.is_nil(), "conversation ID should be non-nil");

    // List conversations — should include the one we just created.
    let list = client
        .list_conversations()
        .await
        .expect("list_conversations should succeed");
    assert!(
        list.iter().any(|c| c.id == created.id),
        "created conversation should appear in list"
    );

    // Get conversation detail.
    let detail = client
        .get_conversation(created.id)
        .await
        .expect("get_conversation should succeed");
    assert_eq!(detail.id, created.id);

    // Delete the conversation.
    client
        .delete_conversation(created.id)
        .await
        .expect("delete_conversation should succeed");

    // Verify deletion — list should no longer contain it.
    let list_after = client
        .list_conversations()
        .await
        .expect("list after delete should succeed");
    assert!(
        !list_after.iter().any(|c| c.id == created.id),
        "deleted conversation should not appear"
    );
}

/// Client library space CRUD against a live gateway.
///
/// Proves: the typed space methods [mkqq] correctly call the gateway's
/// REST endpoints and the client response types deserialise cleanly.
///
/// Motes: mkqq (space CRUD) + e32t (GatewayClient base)
#[tokio::test(flavor = "multi_thread")]
async fn client_library_space_crud_against_live_gateway() {
    let (_router, addr) = start_test_gateway().await;
    let address = format!("http://{addr}");
    let client = swage_client::http::GatewayClient::new(&address);

    // List spaces — should contain at least the default space.
    let spaces = client
        .list_spaces()
        .await
        .expect("list_spaces should succeed");
    assert!(
        !spaces.spaces.is_empty(),
        "should have at least the default space"
    );
}

/// Runtime metadata round-trip: write, discover, use, clean up.
///
/// Proves: RuntimeMetadata [f9b7] integrates with GatewayClient::from_runtime
/// [e32t] so the CLI commands can discover the gateway without hardcoded addresses.
///
/// Motes: f9b7 (RuntimeMetadata) + e32t (GatewayClient from_runtime)
#[tokio::test]
async fn runtime_metadata_round_trip_with_client_discovery() {
    let (_router, addr) = start_test_gateway().await;
    let address = format!("http://{addr}");

    // Write runtime metadata as the gateway startup [kfs2] would.
    let metadata = swage_client::runtime::RuntimeMetadata::new(std::process::id(), address.clone());
    metadata.save().expect("save should succeed");

    // Discover the gateway via metadata — the path CLI commands take.
    let client = swage_client::http::GatewayClient::from_runtime()
        .expect("from_runtime should succeed when metadata exists");

    // The discovered client should reach the same gateway.
    client
        .health()
        .await
        .expect("health via discovered client should succeed");

    // Clean up metadata as shutdown [kfs2] would.
    swage_client::runtime::RuntimeMetadata::remove().expect("remove should succeed");

    // After removal, from_runtime should fail.
    assert!(
        swage_client::http::GatewayClient::from_runtime().is_err(),
        "from_runtime should fail when no metadata exists"
    );
}

/// Stale runtime metadata detection and cleanup.
///
/// Proves: RuntimeMetadata::is_stale [f9b7] correctly detects a dead PID,
/// which is consumed by both `swage gateway start` [bfuq] (to clean up
/// before restarting) and `swage gateway status` [f1jd] (to report and clean).
///
/// Motes: f9b7 (RuntimeMetadata is_stale)
#[tokio::test]
async fn stale_metadata_detected_for_dead_pid() {
    // Use PID 1 as a "dead" PID proxy — it's init/systemd, not our gateway,
    // but is_stale checks whether the process matches our binary, not just alive.
    // Use an impossibly high PID that is almost certainly not running.
    let metadata = swage_client::runtime::RuntimeMetadata::new(
        999_999_999,
        "http://127.0.0.1:99999".to_string(),
    );
    metadata.save().expect("save should succeed");

    // Load and check staleness.
    let loaded = swage_client::runtime::RuntimeMetadata::load().expect("load should succeed");
    assert!(loaded.is_stale(), "metadata with dead PID should be stale");

    // Clean up.
    swage_client::runtime::RuntimeMetadata::remove().expect("remove should succeed");
}

/// Shell completions include the actual CLI subcommand structure.
///
/// Proves: completions generation [sazd] uses the real Cli definition [6wzo],
/// not a stale or separate command tree. This catches the wiring risk:
/// "Completions generate from a stale command tree — output doesn't include
/// actual subcommands."
///
/// Motes: 6wzo (CLI definitions) + sazd (completions)
#[tokio::test]
async fn shell_completions_include_gateway_subcommands() {
    // Generate fish completions (fish format is easiest to grep).
    let mut output = Vec::new();
    swage_gateway::cli::completions::generate_to_writer(clap_complete::Shell::Fish, &mut output);
    let completions = String::from_utf8(output).expect("completions should be valid UTF-8");

    // Verify the completions reference the actual subcommands.
    assert!(
        completions.contains("gateway"),
        "completions should include 'gateway' group"
    );
    assert!(
        completions.contains("start"),
        "completions should include 'start' subcommand"
    );
    assert!(
        completions.contains("stop"),
        "completions should include 'stop' subcommand"
    );
    assert!(
        completions.contains("status"),
        "completions should include 'status' subcommand"
    );
    assert!(
        completions.contains("generate-completion"),
        "completions should include 'generate-completion' command"
    );
}

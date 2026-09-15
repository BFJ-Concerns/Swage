//! The live direction of surface annotation: an agent's note reaches a user's
//! open work surface without a reload.
//!
//! The discriminating shape is a surface with *no conversation selected* —
//! the ordinary case of someone reading a surface while an agent works. The
//! conversation socket cannot serve it (it needs a conversation id, and one
//! connection per conversation), so this test opens the work-surface channel
//! and drives an agent-authored annotation through the real `surface_write`
//! tool, asserting the signal arrives over the wire and carries identity
//! only — never the note's text.

#![allow(clippy::unwrap_used)]

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite;
use uuid::Uuid;

use swage_gateway::server::build_router;
use swage_gateway::state::AppState;
use swage_gateway::tool::Tool;
use swage_gateway::tool_executor::GatewayToolContext;
use swage_gateway::websocket::ServerEvent;
use swage_storage::NewSession;
use swage_storage::SqliteStorage;
use swage_storage::store::{SessionStore, Storage, TopicStore};
use swage_tool::context::MockToolContext;

struct StubProvider;

#[async_trait::async_trait]
impl swage_provider::LlmProvider for StubProvider {
    fn model_id(&self) -> &str {
        "stub"
    }

    async fn complete(
        &self,
        _request: swage_core::CompletionRequest,
    ) -> Result<swage_core::CompletionResponse, swage_core::ProviderError> {
        unimplemented!("no turns are run here")
    }

    async fn complete_with_tools(
        &self,
        _request: swage_core::ToolCompletionRequest,
    ) -> Result<swage_core::ToolCompletionResponse, swage_core::ProviderError> {
        unimplemented!("no turns are run here")
    }

    fn stream(
        &self,
        _request: swage_core::CompletionRequest,
    ) -> std::pin::Pin<Box<dyn futures::Stream<Item = swage_core::StreamChunk> + Send>> {
        unimplemented!("no turns are run here")
    }
}

/// Read frames until one is a `surface_updated` signal, or the wait runs out.
async fn next_surface_signal(
    socket: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
) -> Value {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let frame = tokio::time::timeout_at(deadline, socket.next())
            .await
            .expect("a surface_updated signal should arrive on the work-surface channel")
            .expect("the channel should stay open")
            .expect("frame");
        if let tungstenite::Message::Text(text) = frame {
            let value: Value = serde_json::from_str(&text).expect("json frame");
            if value["type"] == "surface_updated" {
                return value;
            }
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_surface_update_inside_the_upgrade_handshake_window_reaches_the_surface() {
    let storage = Arc::new(SqliteStorage::in_memory().unwrap());
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn TopicStore>,
        Arc::new(StubProvider),
        swage_config::ResolvedConfig::default(),
        swage_gateway::tool::ToolDispatcher::allow_all(vec![]),
        None,
    );
    let surface_id = Uuid::new_v4();
    let broadcast = state.broadcast.clone();
    let router = build_router(state).layer(axum::middleware::from_fn(
        move |request: axum::extract::Request, next: axum::middleware::Next| {
            let broadcast = broadcast.clone();
            async move {
                let response = next.run(request).await;
                // The handler has built the upgrade response, but Hyper cannot
                // complete the handshake until this middleware returns it.
                let _ = broadcast.send(ServerEvent::SurfaceUpdated {
                    surface_id,
                    module_id: None,
                });
                response
            }
        },
    ));

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });

    let (mut socket, _) =
        tokio_tungstenite::connect_async(format!("ws://{addr}/api/work-surface/ws"))
            .await
            .expect("work-surface handshake");
    let signal = next_surface_signal(&mut socket).await;
    assert_eq!(signal["surface_id"], surface_id.to_string());
}

#[tokio::test(flavor = "multi_thread")]
async fn an_agents_annotation_signals_a_surface_with_no_conversation_open() {
    let storage = Arc::new(SqliteStorage::in_memory().unwrap());
    let topic = storage.general_scope().await.unwrap();
    let session = storage
        .create_session(NewSession {
            topic_id: topic.id,
            agent_id: "assistant".into(),
            origin: swage_storage::SessionOrigin::Direct,
            metadata: None,
            active_workspace_attachment_id: None,
        })
        .await
        .unwrap();

    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn TopicStore>,
        Arc::new(StubProvider),
        swage_config::ResolvedConfig::default(),
        swage_gateway::tool::ToolDispatcher::allow_all(vec![]),
        None,
    );

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = build_router(state.clone());
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });

    // The user's surface: it holds no conversation, and never has.
    let (mut socket, _) =
        tokio_tungstenite::connect_async(format!("ws://{addr}/api/work-surface/ws"))
            .await
            .expect("the work-surface channel takes no identity");

    let base = MockToolContext::new("assistant", session.id.to_string());
    let (ws_tx, _ws_rx) = tokio::sync::mpsc::unbounded_channel();
    let ctx = GatewayToolContext::new(&base, ws_tx, Arc::clone(&storage) as Arc<dyn Storage>)
        .with_app_state(Some(state.clone()));

    let write = swage_gateway::tool::surface::SurfaceWriteTool;
    let surface = write
        .execute(json!({ "title": "Release checklist" }), &ctx)
        .await
        .expect("the agent creates the surface");
    let surface_id = surface["id"].as_str().expect("surface id").to_string();
    let creation = next_surface_signal(&mut socket).await;
    assert_eq!(creation["surface_id"], surface_id);

    let module = write
        .execute(
            json!({ "surface_id": surface_id, "source": "Publish the changelog." }),
            &ctx,
        )
        .await
        .expect("the agent writes a module");
    let module_id = module["id"].as_str().expect("module id").to_string();
    let written = next_surface_signal(&mut socket).await;
    assert_eq!(written["module_id"], module_id);

    // The note itself: authored by the agent, hung on that module.
    write
        .execute(
            json!({
                "surface_id": surface_id,
                "module_id": module_id,
                "annotation": "checked this against the tag",
            }),
            &ctx,
        )
        .await
        .expect("the agent leaves a note");

    let signal = next_surface_signal(&mut socket).await;
    assert_eq!(signal["surface_id"], surface_id);
    assert_eq!(
        signal["module_id"], module_id,
        "the signal names the module the note hangs on"
    );
    let frame = serde_json::to_string(&signal).unwrap();
    assert!(
        !frame.contains("checked this against the tag"),
        "the signal carries identity only — the surface reads the note through \
         the surface, never off the event: {frame}"
    );
}

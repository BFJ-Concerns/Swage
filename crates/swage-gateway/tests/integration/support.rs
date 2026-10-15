use std::collections::VecDeque;
use std::ffi::{OsStr, OsString};
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard};

use async_trait::async_trait;
use axum::{Router, http::StatusCode};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use futures::StreamExt;
use serde_json::Value;
use swage_core::{
    CompletionRequest, CompletionResponse, ProviderError, StreamChunk, ToolCompletionRequest,
    ToolCompletionResponse,
};
use swage_provider::LlmProvider;
use swage_storage::MANAGED_SECRET_KEY_ENV;
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite;

pub type WsStream = futures::stream::SplitStream<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
>;

static PROCESS_ENV_LOCK: Mutex<()> = Mutex::new(());

/// Names the parent directory for disposable test roots, for a machine whose
/// system temporary directory is too small for test state.
const TEST_SCRATCH_DIR_ENV: &str = "SWAGE_TEST_SCRATCH_DIR";

/// Create a disposable test root under `SWAGE_TEST_SCRATCH_DIR`, or under the
/// system temporary directory when it is unset.
pub fn scratch_tempdir() -> tempfile::TempDir {
    let mut builder = tempfile::Builder::new();
    builder.prefix("swage-gateway-test-");
    let Some(parent) = std::env::var_os(TEST_SCRATCH_DIR_ENV) else {
        return builder.tempdir().expect("create gateway test root");
    };
    std::fs::create_dir_all(&parent)
        .and_then(|()| builder.tempdir_in(&parent))
        .unwrap_or_else(|error| {
            panic!(
                "create gateway test root under {TEST_SCRATCH_DIR_ENV}={}: {error}",
                std::path::Path::new(&parent).display()
            )
        })
}

/// A streaming provider that records every request and serves scripted rounds.
pub struct CapturingProvider {
    model_id: &'static str,
    rounds: Mutex<VecDeque<Vec<StreamChunk>>>,
    repeated_round: Option<Vec<StreamChunk>>,
    requests: Arc<Mutex<Vec<CompletionRequest>>>,
}

impl CapturingProvider {
    pub fn new(
        model_id: &'static str,
        rounds: Vec<Vec<StreamChunk>>,
    ) -> (Self, Arc<Mutex<Vec<CompletionRequest>>>) {
        let requests = Arc::new(Mutex::new(Vec::new()));
        (
            Self {
                model_id,
                rounds: Mutex::new(rounds.into()),
                repeated_round: None,
                requests: Arc::clone(&requests),
            },
            requests,
        )
    }

    pub fn repeating(
        model_id: &'static str,
        round: Vec<StreamChunk>,
    ) -> (Self, Arc<Mutex<Vec<CompletionRequest>>>) {
        let requests = Arc::new(Mutex::new(Vec::new()));
        (
            Self {
                model_id,
                rounds: Mutex::new(VecDeque::new()),
                repeated_round: Some(round),
                requests: Arc::clone(&requests),
            },
            requests,
        )
    }
}

#[async_trait]
impl LlmProvider for CapturingProvider {
    fn model_id(&self) -> &str {
        self.model_id
    }

    async fn complete(
        &self,
        _request: CompletionRequest,
    ) -> Result<CompletionResponse, ProviderError> {
        unreachable!("capturing provider is configured for streaming tests")
    }

    async fn complete_with_tools(
        &self,
        _request: ToolCompletionRequest,
    ) -> Result<ToolCompletionResponse, ProviderError> {
        unreachable!("capturing provider is configured for streaming tests")
    }

    fn stream(
        &self,
        request: CompletionRequest,
    ) -> Pin<Box<dyn futures::Stream<Item = StreamChunk> + Send>> {
        self.requests
            .lock()
            .expect("captured provider requests")
            .push(request);
        let round = self
            .rounds
            .lock()
            .expect("scripted provider rounds")
            .pop_front()
            .or_else(|| self.repeated_round.clone())
            .expect("missing configured provider round");
        Box::pin(futures::stream::iter(round))
    }
}

/// Serve a gateway router on an ephemeral loopback address.
pub async fn start_live_server(
    router: Router,
) -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind test gateway listener");
    let address = listener
        .local_addr()
        .expect("test gateway listener address");
    let task = tokio::spawn(async move {
        axum::serve(listener, router)
            .await
            .expect("serve test gateway");
    });
    (address, task)
}

/// Own a listening gateway and its filesystem root for one journey.
pub struct LiveTestGateway {
    pub address: std::net::SocketAddr,
    server: tokio::task::JoinHandle<()>,
    _root: tempfile::TempDir,
}

impl LiveTestGateway {
    pub async fn start(router: Router, root: tempfile::TempDir) -> Self {
        let (address, server) = start_live_server(router).await;
        Self {
            address,
            server,
            _root: root,
        }
    }
}

impl Drop for LiveTestGateway {
    fn drop(&mut self) {
        self.server.abort();
    }
}

/// Events and state decide completion; this bound reports a hung journey.
pub fn journey<T>(
    future: impl std::future::Future<Output = T>,
) -> impl std::future::Future<Output = T> {
    let future = Box::pin(future);
    async move {
        tokio::time::timeout(std::time::Duration::from_secs(180), future)
            .await
            .expect("journey hung before its required event or state")
    }
}

pub async fn live_http_json(
    address: std::net::SocketAddr,
    method: reqwest::Method,
    path: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let client = reqwest::Client::new();
    let mut request = client.request(method, format!("http://{address}{path}"));
    if let Some(body) = body {
        request = request.json(&body);
    }
    let response = request.send().await.expect("live gateway request");
    let status = response.status();
    let body = response.json().await.unwrap_or(Value::Null);
    (status, body)
}

/// Returns None on close instead of panicking.
pub async fn ws_recv_lenient(stream: &mut WsStream) -> Option<Value> {
    loop {
        match stream.next().await {
            Some(Ok(tungstenite::Message::Text(text))) => {
                return Some(serde_json::from_str(&text).unwrap_or(Value::Null));
            }
            Some(Ok(tungstenite::Message::Close(_))) => return None,
            None => return None,
            _ => continue,
        }
    }
}

/// Collect WebSocket events through the named terminal event.
pub async fn ws_collect_until(
    stream: &mut WsStream,
    terminal_event: &str,
    timeout: std::time::Duration,
) -> Vec<Value> {
    ws_collect_until_with(stream, terminal_event, timeout, |stream| {
        Box::pin(ws_recv_lenient(stream))
    })
    .await
}

/// Collect WebSocket events through the named terminal event using the suite's
/// own receive semantics.
pub async fn ws_collect_until_with<F>(
    stream: &mut WsStream,
    terminal_event: &str,
    timeout: std::time::Duration,
    receive: F,
) -> Vec<Value>
where
    F: for<'stream> FnMut(
        &'stream mut WsStream,
    ) -> futures::future::BoxFuture<'stream, Option<Value>>,
{
    let mut events = Vec::new();
    tokio::time::timeout(
        timeout,
        receive_through(stream, terminal_event, &mut events, receive),
    )
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {terminal_event}: {events:?}"));
    events
}

/// Await the terminal event; the enclosing journey owns the hang guard.
pub async fn ws_collect_through(stream: &mut WsStream, terminal_event: &str) -> Vec<Value> {
    let mut events = Vec::new();
    receive_through(stream, terminal_event, &mut events, |stream| {
        Box::pin(ws_recv_lenient(stream))
    })
    .await;
    events
}

async fn receive_through<F>(
    stream: &mut WsStream,
    terminal_event: &str,
    events: &mut Vec<Value>,
    mut receive: F,
) where
    F: for<'stream> FnMut(
        &'stream mut WsStream,
    ) -> futures::future::BoxFuture<'stream, Option<Value>>,
{
    loop {
        let event = receive(stream)
            .await
            .unwrap_or_else(|| panic!("websocket closed before {terminal_event}: {events:?}"));
        let reached_terminal = event["type"] == terminal_event;
        events.push(event);
        if reached_terminal {
            return;
        }
    }
}

pub struct ProcessEnvGuard {
    _lock: MutexGuard<'static, ()>,
    saved_vars: Vec<(OsString, Option<OsString>)>,
}

impl ProcessEnvGuard {
    pub fn new() -> Self {
        Self {
            _lock: PROCESS_ENV_LOCK
                .lock()
                .unwrap_or_else(|error| error.into_inner()),
            saved_vars: Vec::new(),
        }
    }

    pub fn set_swage_home(&mut self, path: &std::path::Path) {
        self.set_var("SWAGE_HOME", path);
    }

    pub fn set_managed_secret_key(&mut self, byte: u8) {
        self.set_var(MANAGED_SECRET_KEY_ENV, BASE64.encode([byte; 32]));
    }

    pub fn remove_managed_secret_key(&mut self) {
        self.remove_var(MANAGED_SECRET_KEY_ENV);
    }

    pub fn set_var<K, V>(&mut self, key: K, value: V)
    where
        K: AsRef<OsStr>,
        V: AsRef<OsStr>,
    {
        self.save_original(&key);
        unsafe { std::env::set_var(key, value) };
    }

    pub fn remove_var<K>(&mut self, key: K)
    where
        K: AsRef<OsStr>,
    {
        self.save_original(&key);
        unsafe { std::env::remove_var(key) };
    }

    fn save_original<K>(&mut self, key: K)
    where
        K: AsRef<OsStr>,
    {
        let key = key.as_ref().to_os_string();
        if self.saved_vars.iter().any(|(saved, _)| saved == &key) {
            return;
        }
        self.saved_vars.push((key.clone(), std::env::var_os(key)));
    }
}

impl Drop for ProcessEnvGuard {
    fn drop(&mut self) {
        for (key, value) in self.saved_vars.iter().rev() {
            match value {
                Some(value) => unsafe { std::env::set_var(key, value) },
                None => unsafe { std::env::remove_var(key) },
            }
        }
    }
}

/// Ask the real workflow tools to discover and run a topic-saved workflow.
pub fn workflow_discovery_rounds() -> Vec<Vec<StreamChunk>> {
    vec![
        vec![
            StreamChunk::ToolUse {
                call_id: "list-workflows".into(),
                name: "workflow_list".into(),
                arguments: serde_json::json!({}),
            },
            StreamChunk::Done {
                finish_reason: Some(swage_core::FinishReason::ToolUse),
            },
        ],
        vec![
            StreamChunk::ToolUse {
                call_id: "run-workflow".into(),
                name: "workflow_run".into(),
                arguments: serde_json::json!({"name": "fleet-hold", "wait": true}),
            },
            StreamChunk::Done {
                finish_reason: Some(swage_core::FinishReason::ToolUse),
            },
        ],
        vec![
            StreamChunk::Delta {
                content: "Workflow checked".into(),
            },
            StreamChunk::Done {
                finish_reason: Some(swage_core::FinishReason::Stop),
            },
        ],
    ]
}

/// Read tool results produced by the gateway, never a scripted model's answer.
pub async fn assert_workflow_discovery(storage: &dyn swage_storage::Storage, turn_id: uuid::Uuid) {
    let calls = storage
        .list_tool_calls_by_turn(turn_id)
        .await
        .expect("tool calls");
    let listing = calls
        .iter()
        .find(|call| call.tool == "workflow_list")
        .expect("workflow_list was called");
    assert_eq!(
        listing.status.as_deref(),
        Some("success"),
        "workflow_list failed: {:?}",
        listing.result
    );
    let result: Value = serde_json::from_str(listing.result.as_deref().expect("listing result"))
        .expect("listing JSON");
    let workflows = result["workflows"].as_array().expect("workflow catalogue");
    for name in ["fleet-hold", "research-sweep"] {
        assert!(
            workflows.iter().any(|workflow| workflow["name"] == name),
            "missing {name}: {result}"
        );
    }
    let started = calls
        .iter()
        .find(|call| call.tool == "workflow_run")
        .expect("workflow_run was called");
    assert_eq!(
        started.status.as_deref(),
        Some("success"),
        "workflow_run failed: {:?}",
        started.result
    );
    let result: Value =
        serde_json::from_str(started.result.as_deref().expect("run result")).expect("run JSON");
    let run_id = result["id"]
        .as_str()
        .expect("run id")
        .parse()
        .expect("run UUID");
    let run = storage
        .get_workflow_run(run_id)
        .await
        .expect("run lookup")
        .expect("persisted workflow run");
    assert_eq!(run.state, "completed");
    let turn = storage
        .get_turn(turn_id)
        .await
        .expect("turn lookup")
        .expect("persisted turn");
    let thread = swage_storage::ThreadStore::get(storage, turn.thread_id)
        .await
        .expect("thread lookup")
        .expect("persisted thread");
    assert_eq!(run.dispatching_session_id, Some(thread.session_id));
}

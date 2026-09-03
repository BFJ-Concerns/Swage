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

/// Create a disposable test root off the nearly-full system volume.
pub fn scratch_tempdir() -> tempfile::TempDir {
    match tempfile::Builder::new()
        .prefix("swage-gateway-test-")
        .tempdir_in("/tmp/scratch")
    {
        Ok(tempdir) => tempdir,
        Err(error) => {
            eprintln!(
                "could not create gateway test root in /tmp/scratch ({error}); falling back to the system temporary directory"
            );
            tempfile::tempdir().expect("create gateway test root")
        }
    }
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
    mut receive: F,
) -> Vec<Value>
where
    F: for<'stream> FnMut(
        &'stream mut WsStream,
    ) -> futures::future::BoxFuture<'stream, Option<Value>>,
{
    let mut events = Vec::new();
    tokio::time::timeout(timeout, async {
        loop {
            let event = receive(stream)
                .await
                .unwrap_or_else(|| panic!("websocket closed before {terminal_event}: {events:?}"));
            let reached_terminal = event["type"] == terminal_event;
            events.push(event);
            if reached_terminal {
                break;
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {terminal_event}: {events:?}"));
    events
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

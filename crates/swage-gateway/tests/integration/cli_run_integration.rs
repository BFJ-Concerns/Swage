#![allow(clippy::unwrap_used)]

use std::io::Write;
use std::net::SocketAddr;
use std::pin::Pin;
use std::process::{Command, Stdio};
use std::sync::Arc;

use async_trait::async_trait;
use axum::Router;

use swage_core::{
    CompletionRequest, CompletionResponse, ProviderError, StreamChunk, ToolCompletionRequest,
    ToolCompletionResponse,
};
use swage_gateway::{server::build_router, state::AppState};
use swage_provider::{
    LlmProvider, SpeechRequest, SpeechResponse, TranscriptionRequest, TranscriptionResponse,
    VoiceCapabilities, VoiceProvider,
};
use swage_storage::SqliteStorage;

use crate::support::start_live_server;

struct MockProvider {
    chunks: std::sync::Mutex<Vec<StreamChunk>>,
}

impl MockProvider {
    fn new(chunks: Vec<StreamChunk>) -> Self {
        Self {
            chunks: std::sync::Mutex::new(chunks),
        }
    }
}

#[async_trait]
impl LlmProvider for MockProvider {
    fn model_id(&self) -> &str {
        "cli-run-mock"
    }

    async fn complete(
        &self,
        _request: CompletionRequest,
    ) -> Result<CompletionResponse, ProviderError> {
        unreachable!("cli run integration uses streaming only")
    }

    async fn complete_with_tools(
        &self,
        _request: ToolCompletionRequest,
    ) -> Result<ToolCompletionResponse, ProviderError> {
        unreachable!("cli run integration uses streaming only")
    }

    fn stream(
        &self,
        _request: CompletionRequest,
    ) -> Pin<Box<dyn futures::Stream<Item = StreamChunk> + Send>> {
        let chunks = std::mem::take(&mut *self.chunks.lock().unwrap());
        Box::pin(futures::stream::iter(chunks))
    }
}

struct MockVoiceProvider;

#[async_trait]
impl VoiceProvider for MockVoiceProvider {
    fn capabilities(&self) -> VoiceCapabilities {
        VoiceCapabilities {
            transcription: true,
            synthesis: true,
            default_voice: Some("nova".to_string()),
            supported_voices: vec!["nova".to_string()],
            unavailable_reason: None,
        }
    }

    async fn transcribe(
        &self,
        request: TranscriptionRequest,
    ) -> Result<TranscriptionResponse, ProviderError> {
        assert_eq!(request.mime_type, "audio/wav");
        Ok(TranscriptionResponse {
            text: String::from_utf8(request.audio_bytes).unwrap(),
        })
    }

    async fn synthesise(&self, request: SpeechRequest) -> Result<SpeechResponse, ProviderError> {
        Ok(SpeechResponse {
            audio_bytes: format!(
                "voice={} format={} text={}",
                request.voice.unwrap_or_else(|| "nova".to_string()),
                request.format.unwrap_or_else(|| "wav".to_string()),
                request.text
            )
            .into_bytes(),
            mime_type: "audio/wav".to_string(),
            voice: "nova".to_string(),
        })
    }
}

async fn start_test_server(
    provider: impl LlmProvider + 'static,
    voice_provider: Option<Arc<dyn VoiceProvider>>,
) -> SocketAddr {
    let storage = Arc::new(SqliteStorage::in_memory().unwrap());
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>,
        Arc::clone(&storage) as Arc<dyn swage_storage::store::TopicStore>,
        Arc::new(provider),
        swage_config::ResolvedConfig::default(),
        swage_gateway::tool::ToolDispatcher::allow_all(vec![]),
        None,
    );
    if let Some(voice_provider) = voice_provider {
        state.replace_voice_provider(voice_provider);
    }
    let router: Router = build_router(state);

    start_live_server(router).await.0
}

#[tokio::test(flavor = "multi_thread")]
async fn run_command_streams_response_from_argument_prompt() {
    let addr = start_test_server(
        MockProvider::new(vec![
            StreamChunk::Delta {
                content: "hello".into(),
            },
            StreamChunk::Delta {
                content: " cli".into(),
            },
            StreamChunk::Done {
                finish_reason: None,
            },
        ]),
        None,
    )
    .await;

    let output = Command::new(env!("CARGO_BIN_EXE_swage"))
        .args(["run", "--url", &format!("http://{addr}"), "Say hello"])
        .output()
        .expect("run command should execute");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "hello cli\n");
}

#[tokio::test(flavor = "multi_thread")]
async fn run_command_reads_prompt_from_stdin() {
    let addr = start_test_server(
        MockProvider::new(vec![
            StreamChunk::Delta {
                content: "stdin".into(),
            },
            StreamChunk::Delta {
                content: " path".into(),
            },
            StreamChunk::Done {
                finish_reason: None,
            },
        ]),
        None,
    )
    .await;

    let mut child = Command::new(env!("CARGO_BIN_EXE_swage"))
        .args(["run", "--url", &format!("http://{addr}")])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("run command should spawn");

    let mut stdin = child.stdin.take().expect("stdin pipe");
    stdin.write_all(b"Prompt from stdin\n").unwrap();
    drop(stdin);

    let output = child.wait_with_output().expect("wait_with_output");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "stdin path\n");
}

#[tokio::test(flavor = "multi_thread")]
async fn run_command_supports_audio_input_and_output() {
    let addr = start_test_server(
        MockProvider::new(vec![
            StreamChunk::Delta {
                content: "spoken".into(),
            },
            StreamChunk::Delta {
                content: " reply".into(),
            },
            StreamChunk::Done {
                finish_reason: None,
            },
        ]),
        Some(Arc::new(MockVoiceProvider)),
    )
    .await;

    let dir = tempfile::tempdir().unwrap();
    let input_path = dir.path().join("input.wav");
    let output_path = dir.path().join("reply.wav");
    std::fs::write(&input_path, b"Prompt from audio").unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_swage"))
        .args([
            "run",
            "--url",
            &format!("http://{addr}"),
            "--audio-input",
            input_path.to_str().unwrap(),
            "--audio-output",
            output_path.to_str().unwrap(),
            "--voice",
            "nova",
        ])
        .output()
        .expect("run command should execute");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "spoken reply\n");
    assert_eq!(
        std::fs::read_to_string(&output_path).unwrap(),
        "voice=nova format=wav text=spoken reply"
    );
}

#[test]
fn run_command_returns_non_zero_when_gateway_is_unreachable() {
    let output = Command::new(env!("CARGO_BIN_EXE_swage"))
        .args(["run", "--url", "http://127.0.0.1:1", "will fail"])
        .output()
        .expect("run command should execute");

    assert!(!output.status.success(), "command should fail");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("Error:"),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn run_command_rejects_audio_input_when_voice_support_is_unavailable() {
    let addr = start_test_server(
        MockProvider::new(vec![StreamChunk::Done {
            finish_reason: None,
        }]),
        None,
    )
    .await;

    let dir = tempfile::tempdir().unwrap();
    let input_path = dir.path().join("input.wav");
    std::fs::write(&input_path, b"Prompt from audio").unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_swage"))
        .args([
            "run",
            "--url",
            &format!("http://{addr}"),
            "--audio-input",
            input_path.to_str().unwrap(),
        ])
        .output()
        .expect("run command should execute");

    assert!(!output.status.success(), "command should fail");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("voice transcription"),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cli_pairs_once_and_remembers_headers_for_http_and_websocket() {
    use axum::{extract::ConnectInfo, middleware};
    use swage_gateway::access_gate::AccessGate;
    let root = crate::support::scratch_tempdir();
    let client_root = root.path().join("client");
    let storage = Arc::new(SqliteStorage::in_memory().unwrap());
    let provider = Arc::new(MockProvider::new(vec![]));
    let mut config = swage_config::ResolvedConfig::default();
    config.sandbox.enabled = false;
    let mut state = AppState::new_with_config_root(
        storage.clone(),
        storage,
        provider.clone(),
        config,
        swage_gateway::tool::ToolDispatcher::allow_all(vec![]),
        None,
        root.path().join("gateway"),
    );
    std::fs::create_dir_all(&state.config_root).unwrap();
    state.access_gate = Arc::new(AccessGate::for_binding(&state.config_root, "0.0.0.0").unwrap());
    let code = state.access_gate.issue_pairing_code().pairing_code;
    let observed = Arc::new(std::sync::Mutex::new(Vec::new()));
    let capture = observed.clone();
    // The transport is loopback for isolation; exercise the LAN admission path
    // by presenting the same remote peer to both HTTP and upgrade middleware.
    let router = build_router(state).layer(middleware::from_fn(
        move |mut request: axum::extract::Request, next: middleware::Next| {
            let capture = capture.clone();
            async move {
                capture.lock().unwrap().push((
                    request.uri().path().to_string(),
                    request
                        .headers()
                        .get("X-Swage-Token")
                        .and_then(|value| value.to_str().ok())
                        .map(str::to_owned),
                ));
                request.extensions_mut().insert(ConnectInfo(
                    "192.0.2.10:4567".parse::<SocketAddr>().unwrap(),
                ));
                next.run(request).await
            }
        },
    ));
    let address = format!("http://{}", start_live_server(router).await.0);
    for first in [true, false] {
        *provider.chunks.lock().unwrap() = vec![
            StreamChunk::Delta {
                content: "paired".into(),
            },
            StreamChunk::Done {
                finish_reason: None,
            },
        ];
        let mut command = Command::new(env!("CARGO_BIN_EXE_swage"));
        command
            .env("SWAGE_HOME", &client_root)
            .env("XDG_RUNTIME_DIR", root.path().join("run"))
            .args(["run", "--url", &address]);
        if first {
            command.args(["--pairing-code", &code]);
        }
        let output = command.arg("hello").stdin(Stdio::null()).output().unwrap();
        assert!(
            output.status.success(),
            "CLI run refused: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(String::from_utf8_lossy(&output.stdout), "paired\n");
    }
    let requests = observed.lock().unwrap();
    assert_eq!(
        requests
            .iter()
            .filter(|(path, _)| path == "/api/pair")
            .count(),
        1
    );
    let conversation_requests: Vec<_> = requests
        .iter()
        .filter(|(path, _)| path.starts_with("/api/conversations"))
        .collect();
    assert_eq!(
        conversation_requests.len(),
        4,
        "two creates and two upgrades"
    );
    assert!(
        conversation_requests
            .iter()
            .all(|(_, token)| token.is_some()),
        "every conversation request must carry the remembered token"
    );
}

async fn run_unpaired_without_terminal(
    mode: swage_protocol::client::access::AccessGateMode,
) -> (std::process::Output, Vec<String>) {
    use axum::{extract::ConnectInfo, middleware};
    use swage_gateway::access_gate::AccessGate;
    let root = crate::support::scratch_tempdir();
    let storage = Arc::new(SqliteStorage::in_memory().unwrap());
    let mut config = swage_config::ResolvedConfig::default();
    config.sandbox.enabled = false;
    config.server.access = mode;
    let mut state = AppState::new_with_config_root(
        storage.clone(),
        storage,
        Arc::new(MockProvider::new(vec![
            StreamChunk::Delta {
                content: "proceeded".into(),
            },
            StreamChunk::Done {
                finish_reason: None,
            },
        ])),
        config,
        swage_gateway::tool::ToolDispatcher::allow_all(vec![]),
        None,
        root.path().join("gateway"),
    );
    std::fs::create_dir_all(&state.config_root).unwrap();
    state.access_gate = Arc::new(AccessGate::for_binding(&state.config_root, "0.0.0.0").unwrap());
    let paths = Arc::new(std::sync::Mutex::new(Vec::new()));
    let capture = paths.clone();
    let router = build_router(state).layer(middleware::from_fn(
        move |mut request: axum::extract::Request, next: middleware::Next| {
            let capture = capture.clone();
            async move {
                capture
                    .lock()
                    .unwrap()
                    .push(request.uri().path().to_owned());
                request.extensions_mut().insert(ConnectInfo(
                    "192.0.2.10:4567".parse::<SocketAddr>().unwrap(),
                ));
                next.run(request).await
            }
        },
    ));
    let address = format!("http://{}", start_live_server(router).await.0);
    let access: serde_json::Value = reqwest::get(format!("{address}/api/access"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(access["admitted"], false);
    let mut child = Command::new("setsid")
        .arg(env!("CARGO_BIN_EXE_swage"))
        .args(["run", "--url", &address])
        .env("SWAGE_HOME", root.path().join("client"))
        .env("XDG_RUNTIME_DIR", root.path().join("runtime"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"summarise this\n")
        .unwrap();
    let output = child.wait_with_output().unwrap();
    let paths = paths.lock().unwrap().clone();
    (output, paths)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cli_warn_mode_does_not_require_pairing() {
    let (output, paths) =
        run_unpaired_without_terminal(swage_protocol::client::access::AccessGateMode::Warn).await;
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "warn mode must proceed without pairing: {stderr}"
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "proceeded\n");
    assert!(!stderr.contains("Pair this client"));
    assert!(!paths.iter().any(|path| path == "/api/pair"));
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cli_token_mode_with_piped_prompt_requires_explicit_pairing_without_terminal() {
    let (output, paths) =
        run_unpaired_without_terminal(swage_protocol::client::access::AccessGateMode::Token).await;
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success());
    assert!(
        stderr.contains("--pairing-code"),
        "missing named refusal: {stderr}"
    );
    assert!(
        !paths.iter().any(|path| path == "/api/pair"),
        "prompt must not be submitted as a pairing code"
    );
}

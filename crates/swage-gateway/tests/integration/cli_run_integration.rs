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
            synthesis_unverified: false,
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
) -> (SocketAddr, tempfile::TempDir) {
    let storage = Arc::new(SqliteStorage::in_memory().unwrap());
    let root = tempfile::tempdir().unwrap();
    let state = AppState::new_with_config_root(
        Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>,
        Arc::clone(&storage) as Arc<dyn swage_storage::store::TopicStore>,
        Arc::new(provider),
        swage_config::ResolvedConfig::default(),
        swage_gateway::tool::ToolDispatcher::allow_all(vec![]),
        None,
        root.path().to_path_buf(),
    );
    if let Some(voice_provider) = voice_provider {
        state.replace_voice_provider(voice_provider);
    }
    let router: Router = build_router(state);

    (start_live_server(router).await.0, root)
}

#[tokio::test(flavor = "multi_thread")]
async fn run_command_streams_response_from_argument_prompt() {
    let (addr, _root) = start_test_server(
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
    let (addr, _root) = start_test_server(
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
    let (addr, _root) = start_test_server(
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
    let (addr, _root) = start_test_server(
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

#[tokio::test(flavor = "multi_thread")]
async fn run_audio_output_checks_fresh_speech_and_returns_provider_failures() {
    use axum::{http::StatusCode, routing::post};
    use swage_provider::{OpenAiVoiceProvider, ProviderConfig, voice::VoiceModelRoutes};

    for (status, body, remains_unchecked) in [
        (StatusCode::OK, "RIFF working audio", false),
        (StatusCode::NOT_FOUND, "speech model missing upstream", true),
        (
            StatusCode::NOT_FOUND,
            r#"{"error":{"code":"model_not_found"}}"#,
            false,
        ),
    ] {
        let upstream = start_live_server(
            Router::new().route("/audio/speech", post(move || async move { (status, body) })),
        )
        .await
        .0;
        let voice = Arc::new(OpenAiVoiceProvider::new(
            ProviderConfig::new("key", "chat", format!("http://{upstream}")),
            VoiceModelRoutes {
                speech_model: Some("speech".into()),
                transcription_model: None,
            },
        ));
        assert!(!voice.capabilities().synthesis);
        let (addr, _root) = start_test_server(
            MockProvider::new(vec![
                StreamChunk::Delta {
                    content: "hello".into(),
                },
                StreamChunk::Done {
                    finish_reason: None,
                },
            ]),
            Some(voice.clone()),
        )
        .await;
        let unchecked: serde_json::Value =
            reqwest::get(format!("http://{addr}/api/voice/capabilities"))
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("reply.wav");
        let output = Command::new(env!("CARGO_BIN_EXE_swage"))
            .env("SWAGE_HOME", dir.path())
            .env("XDG_RUNTIME_DIR", dir.path())
            .args([
                "run",
                "--url",
                &format!("http://{addr}"),
                "--audio-output",
                path.to_str().unwrap(),
                "hello",
            ])
            .output()
            .unwrap();
        if status.is_success() {
            assert!(
                output.status.success(),
                "fresh speech must write audio: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(unchecked["synthesis_unverified"], true);
            assert_eq!(unchecked["synthesis"], false);
            assert_eq!(std::fs::read(&path).unwrap(), body.as_bytes());
            assert!(voice.capabilities().synthesis);
        } else {
            assert!(!output.status.success());
            assert!(
                String::from_utf8_lossy(&output.stderr).contains(body),
                "CLI must return the provider's reason: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(!path.exists());
            assert!(!voice.capabilities().synthesis);
        }
        let checked: serde_json::Value =
            reqwest::get(format!("http://{addr}/api/voice/capabilities"))
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
        assert_eq!(checked["synthesis_unverified"], remains_unchecked);
        assert_eq!(checked["synthesis"], status.is_success());
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn run_audio_output_rechecks_unverified_speech_but_refuses_stored_failures() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use axum::{http::StatusCode, routing::post};
    use swage_provider::{OpenAiVoiceProvider, ProviderConfig, voice::VoiceModelRoutes};

    for failed in [false, true] {
        let requests = Arc::new(AtomicUsize::new(0));
        let count = requests.clone();
        let upstream = start_live_server(Router::new().route(
            "/audio/speech",
            post(move || {
                let index = count.fetch_add(1, Ordering::SeqCst);
                async move {
                    match index {
                        0 if failed => {
                            (StatusCode::UNAUTHORIZED, "original authentication failure")
                        }
                        1 => (StatusCode::BAD_REQUEST, "Voice example does not exist"),
                        _ => (StatusCode::OK, "RIFF working audio"),
                    }
                }
            }),
        ))
        .await
        .0;
        let voice = Arc::new(OpenAiVoiceProvider::new(
            ProviderConfig::new("key", "chat", format!("http://{upstream}")),
            VoiceModelRoutes {
                speech_model: Some("speech".into()),
                transcription_model: Some("transcription".into()),
            },
        ));
        let request = || SpeechRequest {
            text: "hello".into(),
            voice: None,
            format: None,
        };
        assert_eq!(voice.synthesise(request()).await.is_err(), failed);
        voice.synthesise(request()).await.unwrap_err();
        let before = voice.capabilities();
        assert!(!before.synthesis);
        assert_eq!(before.synthesis_unverified, !failed);
        let (addr, _root) = start_test_server(
            MockProvider::new(vec![
                StreamChunk::Delta {
                    content: "hello".into(),
                },
                StreamChunk::Done {
                    finish_reason: None,
                },
            ]),
            Some(voice.clone()),
        )
        .await;
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("reply.wav");
        let output = Command::new(env!("CARGO_BIN_EXE_swage"))
            .env("SWAGE_HOME", root.path())
            .env("XDG_RUNTIME_DIR", root.path())
            .args([
                "run",
                "--url",
                &format!("http://{addr}"),
                "--audio-output",
                path.to_str().unwrap(),
                "hello",
            ])
            .output()
            .unwrap();
        if failed {
            assert!(!output.status.success());
            assert!(
                String::from_utf8_lossy(&output.stderr)
                    .contains(before.unavailable_reason.as_deref().unwrap())
            );
            assert!(!path.exists());
            assert_eq!(voice.capabilities(), before);
            assert_eq!(
                requests.load(Ordering::SeqCst),
                2,
                "known failure must be refused before another speech request"
            );
        } else {
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(std::fs::read(&path).unwrap(), b"RIFF working audio");
            assert!(voice.capabilities().synthesis);
            assert!(!voice.capabilities().synthesis_unverified);
            assert_eq!(voice.capabilities().unavailable_reason, None);
            assert_eq!(
                requests.load(Ordering::SeqCst),
                3,
                "unchecked speech must be requested again"
            );
        }
    }
}

use std::io::Write;
use std::net::SocketAddr;
use std::pin::Pin;
use std::process::{Command, Stdio};
use std::sync::Arc;

use async_trait::async_trait;
use axum::Router;
use tokio::net::TcpListener;

use swage_core::{
    CompletionRequest, CompletionResponse, ProviderError, StreamChunk, ToolCompletionRequest,
    ToolCompletionResponse,
};
use swage_gateway::{server::build_router, state::AppState};
use swage_provider::LlmProvider;
use swage_storage::SqliteStorage;

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

async fn start_test_server(provider: impl LlmProvider + 'static) -> SocketAddr {
    let storage = Arc::new(SqliteStorage::in_memory().unwrap());
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>,
        Arc::clone(&storage) as Arc<dyn swage_storage::store::ProjectStore>,
        Arc::new(provider),
        swage_config::ResolvedConfig::default(),
        swage_gateway::tool::ToolDispatcher::allow_all(vec![]),
        None,
        None,
    );
    let router: Router = build_router(state);

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    addr
}

#[tokio::test(flavor = "multi_thread")]
async fn run_command_streams_response_from_argument_prompt() {
    let addr = start_test_server(MockProvider::new(vec![
        StreamChunk::Delta {
            content: "hello".into(),
        },
        StreamChunk::Delta {
            content: " cli".into(),
        },
        StreamChunk::Done {
            finish_reason: None,
        },
    ]))
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
    let addr = start_test_server(MockProvider::new(vec![
        StreamChunk::Delta {
            content: "stdin".into(),
        },
        StreamChunk::Delta {
            content: " path".into(),
        },
        StreamChunk::Done {
            finish_reason: None,
        },
    ]))
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

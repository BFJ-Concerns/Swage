use std::collections::HashSet;
use std::collections::VecDeque;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::Stdio;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use async_trait::async_trait;
use axum::Router;
use axum::body::{Body, to_bytes};
use axum::extract::State;
use axum::http::{HeaderMap, Request, StatusCode};
use axum::response::{Html, IntoResponse, Redirect};
use axum::routing::get;
use chromiumoxide::Browser;
use chromiumoxide::cdp::browser_protocol::accessibility::{EnableParams, GetFullAxTreeParams};
use chromiumoxide::cdp::browser_protocol::dom::BackendNodeId;
use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use swage_config::{BrowserScreenshotPolicy, BrowserToolConfig, ToolPermissionConfig};
use swage_core::{
    CompletionRequest, CompletionResponse, FinishReason, ProviderError, StreamChunk,
    ToolCompletionRequest, ToolCompletionResponse,
};
use swage_gateway::browser::{BrowserConfig, BrowserHost, render_snapshot};
use swage_gateway::secret::{CreateSecretRequest, SecretManager};
use swage_gateway::{server::build_router, state::AppState};
use swage_provider::LlmProvider;
use swage_storage::SqliteStorage;
use swage_storage::store::{AuditStore, SpaceStore, ToolCallStore};
use swage_tool::permission::{PermissionConfig, check_permission};
use tempfile::TempDir;
use tokio::net::TcpListener;
use tokio::process::Child;
use tokio_tungstenite::tungstenite;
use tower::ServiceExt;

use crate::support::ProcessEnvGuard;
use tokio_util::sync::CancellationToken;

static BROWSER_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn real_browser_drives_fixture_and_persists_profile() {
    let _browser_guard = BROWSER_TEST_LOCK.lock().await;
    let Some(binary) = browser_binary_or_skip() else {
        return;
    };
    let fixture = FixtureServer::spawn().await;
    let temp = TempDir::new().expect("tempdir");
    let host = BrowserHost::new(browser_config(&binary, temp.path(), fixture.host()));

    let form_url = fixture.url("/login");
    let first = tokio::time::timeout(
        std::time::Duration::from_secs(15),
        host.navigate("session-a", &form_url),
    )
    .await
    .expect("navigate login page timed out")
    .expect("navigate login page");
    assert!(
        first
            .refs
            .values()
            .any(|entry| entry.backend_node_id != BackendNodeId::new(0)),
        "expected served login fixture to produce AX-backed refs, got DOM fallback refs:\n{}",
        first.output
    );
    assert!(first.output.contains("textbox \"Username\""));
    assert!(first.output.contains("link \"Sign in\""));

    let username = first
        .refs
        .values()
        .find(|entry| entry.name == "Username")
        .expect("username ref")
        .reference
        .clone();
    // Every action returns a fresh observation with fresh refs, and refs
    // from a superseded snapshot are rejected — so the journey re-reads its
    // targets from each action's returned snapshot, exactly as a model must.
    let (after_username, _) = tokio::time::timeout(
        std::time::Duration::from_secs(15),
        host.type_text("session-a", &username, "ben", true, false),
    )
    .await
    .expect("type username timed out")
    .expect("type username");
    let password = after_username
        .refs
        .values()
        .find(|entry| entry.name == "Password")
        .expect("password ref")
        .reference
        .clone();
    let (after_password, _) = tokio::time::timeout(
        std::time::Duration::from_secs(15),
        host.type_text("session-a", &password, "secret", true, false),
    )
    .await
    .expect("type password timed out")
    .expect("type password");
    let submit = after_password
        .refs
        .values()
        .find(|entry| entry.name == "Sign in")
        .expect("submit ref")
        .reference
        .clone();
    assert!(
        !after_password.output.contains("secret"),
        "password text leaked into post-type snapshot:\n{}",
        after_password.output
    );
    let (after_submit, audit) = tokio::time::timeout(
        std::time::Duration::from_secs(15),
        host.click("session-a", &submit),
    )
    .await
    .expect("click submit timed out")
    .expect("click submit");
    assert!(
        after_submit.output.contains("heading \"Signed in\""),
        "expected signed-in heading after submit, got:\n{}",
        after_submit.output
    );
    assert!(audit.navigated);
    assert!(fixture.saw_login.load(Ordering::SeqCst));

    let same_process_whoami = tokio::time::timeout(
        std::time::Duration::from_secs(15),
        host.navigate("session-a", &fixture.url("/whoami")),
    )
    .await
    .expect("same-process whoami timed out")
    .expect("same-process whoami");
    assert!(
        same_process_whoami.output.contains("Signed in as ben"),
        "expected login cookie before browser restart, got:\n{}",
        same_process_whoami.output
    );

    host.shutdown().await;

    let relaunched = BrowserHost::new(browser_config(&binary, temp.path(), fixture.host()));
    let whoami = relaunched
        .navigate("session-b", &fixture.url("/whoami"))
        .await
        .expect("navigate whoami after restart");
    assert!(
        whoami.output.contains("Signed in as ben"),
        "expected persisted login after restart, got:\n{}",
        whoami.output
    );
    relaunched.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn served_fixture_get_full_ax_tree_is_rich() {
    let _browser_guard = BROWSER_TEST_LOCK.lock().await;
    let Some(binary) = browser_binary_or_skip() else {
        return;
    };
    let fixture = FixtureServer::spawn().await;
    let temp = TempDir::new().expect("tempdir");
    let port = reserve_probe_port();
    let (mut browser, handler, mut child) = spawn_probe_browser(&binary, temp.path(), port).await;
    let page = browser
        .new_page("about:blank")
        .await
        .expect("new probe page");
    page.goto(fixture.url("/login")).await.expect("probe goto");
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    page.execute(EnableParams::default())
        .await
        .expect("enable AX");
    let nodes = page
        .execute(GetFullAxTreeParams::builder().depth(100).build())
        .await
        .expect("get full AX tree")
        .result
        .nodes;
    let roles = nodes
        .iter()
        .filter_map(|node| node.role.as_ref())
        .filter_map(|role| role.value.as_ref())
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    assert!(
        nodes.len() > 10 && roles.iter().any(|role| role.contains("textbox")),
        "expected rich AX tree for served fixture, got {} nodes and roles {roles:?}",
        nodes.len()
    );
    let rendered = render_snapshot(
        &nodes,
        &fixture.url("/login"),
        "Fixture Login",
        1,
        40_000,
        &HashSet::new(),
    );
    assert!(
        rendered.refs.values().any(|entry| entry.name == "Username"),
        "renderer did not produce actionable AX refs from rich tree:\n{}",
        rendered.output
    );
    maybe_record_ax_fixture("login", &nodes, &rendered.output);
    browser.close().await.expect("close probe browser");
    handler.abort();
    let _ = child.kill().await;
}

#[test]
fn recorded_ax_fixtures_render_to_goldens() {
    let fixture_dir = ax_fixture_dir();
    let mut fixtures = std::fs::read_dir(&fixture_dir)
        .unwrap_or_else(|error| {
            panic!(
                "failed to read AX fixture directory '{}': {error}",
                fixture_dir.display()
            )
        })
        .filter_map(|entry| {
            let path = entry.expect("fixture entry").path();
            (path.extension().and_then(|ext| ext.to_str()) == Some("json")).then_some(path)
        })
        .collect::<Vec<_>>();
    fixtures.sort();
    assert!(
        !fixtures.is_empty(),
        "expected at least one committed AX fixture in {}",
        fixture_dir.display()
    );

    for fixture in fixtures {
        let nodes = read_ax_nodes(&fixture);
        let rendered = render_snapshot(
            &nodes,
            &format!(
                "https://fixture.local/{}",
                fixture.file_stem().and_then(|stem| stem.to_str()).unwrap()
            ),
            "Fixture Login",
            1,
            40_000,
            &HashSet::new(),
        );
        let golden = fixture.with_extension("rendered");
        let expected = std::fs::read_to_string(&golden).unwrap_or_else(|error| {
            panic!("failed to read golden '{}': {error}", golden.display())
        });
        assert_eq!(
            rendered.output.trim_end(),
            expected.trim_end(),
            "rendered AX fixture did not match golden for {}",
            fixture.display()
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn browser_reports_dynamic_stale_refs() {
    let _browser_guard = BROWSER_TEST_LOCK.lock().await;
    let Some(binary) = browser_binary_or_skip() else {
        return;
    };
    let fixture = FixtureServer::spawn().await;
    let temp = TempDir::new().expect("tempdir");
    let host = BrowserHost::new(browser_config(&binary, temp.path(), fixture.host()));

    let login = host
        .navigate("stale-session", &fixture.url("/login"))
        .await
        .expect("navigate login");
    let old_ref = login
        .refs
        .values()
        .find(|entry| entry.name == "Password")
        .expect("password ref")
        .reference
        .clone();
    host.navigate("stale-session", &fixture.url("/single-action"))
        .await
        .expect("navigate single action");
    let old_ref_error = host
        .click("stale-session", &old_ref)
        .await
        .expect_err("old snapshot ref should be rejected");
    assert!(
        old_ref_error.to_string().contains("has superseded"),
        "unexpected old-ref error: {old_ref_error}"
    );

    let mutating = host
        .navigate("stale-session", &fixture.url("/mutate"))
        .await
        .expect("navigate mutating page");
    let doomed_ref = mutating
        .refs
        .values()
        .find(|entry| entry.name == "Remove me")
        .expect("doomed ref")
        .reference
        .clone();
    tokio::time::sleep(std::time::Duration::from_millis(1_200)).await;
    let gone_error = host
        .click("stale-session", &doomed_ref)
        .await
        .expect_err("removed element should fail resolution");
    assert!(
        gone_error
            .to_string()
            .contains("that element is no longer on the page"),
        "unexpected gone-element error: {gone_error}"
    );
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn browser_recovers_after_process_crash_with_lost_state_error() {
    let _browser_guard = BROWSER_TEST_LOCK.lock().await;
    let Some(binary) = browser_binary_or_skip() else {
        return;
    };
    let fixture = FixtureServer::spawn().await;
    let temp = TempDir::new().expect("tempdir");
    let host = BrowserHost::new(browser_config(&binary, temp.path(), fixture.host()));

    host.navigate("crash-session", &fixture.url("/login"))
        .await
        .expect("navigate before crash");
    let pid = host
        .browser_pid_for_tests()
        .await
        .expect("browser process should be running");
    unsafe {
        libc::kill(pid as libc::pid_t, libc::SIGKILL);
    }
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    let crash_error = host
        .navigate("crash-session", &fixture.url("/whoami"))
        .await
        .expect_err("first call after browser crash should report lost state");
    assert!(
        crash_error
            .to_string()
            .contains("the browser restarted and page state was lost"),
        "unexpected crash recovery error: {crash_error}"
    );

    let recovered = host
        .navigate("crash-session", &fixture.url("/login"))
        .await
        .expect("fresh navigate after crash should respawn browser");
    assert!(
        recovered.output.contains("heading \"Sign in\""),
        "expected recovered browser to navigate freshly, got:\n{}",
        recovered.output
    );
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn browser_tools_run_through_dispatcher_storage_and_secret_hygiene() {
    let _browser_guard = BROWSER_TEST_LOCK.lock().await;
    let Some(binary) = browser_binary_or_skip() else {
        return;
    };
    let fixture = FixtureServer::spawn().await;
    let temp = TempDir::new().expect("tempdir");
    let mut env_guard = ProcessEnvGuard::new();
    env_guard.set_swage_home(temp.path());
    env_guard.set_managed_secret_key(42);
    let mut browser_config = browser_config_with_screenshots(
        &binary,
        temp.path(),
        fixture.host(),
        BrowserScreenshotPolicy::default(),
    );
    browser_config.max_snapshot_bytes = 700;
    let browser_host = BrowserHost::new(browser_config);
    let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
    let secret = SecretManager::new(Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>)
        .create_secret(CreateSecretRequest {
            name: "Fixture password".into(),
            material_kind: "password".into(),
            owner_kind: "user".into(),
            backend_kind: "managed".into(),
            scope_kind: "gateway".into(),
            scope_id: None,
            metadata: None,
            value: Some("resolved-fixture-password".into()),
            external_reference: None,
        })
        .await
        .expect("create secret");
    let (tx1, rx1) = tokio::sync::mpsc::channel(8);
    let (tx2, rx2) = tokio::sync::mpsc::channel(8);
    let (tx3, rx3) = tokio::sync::mpsc::channel(8);
    let provider = ChannelProvider::new(vec![rx1, rx2, rx3]);
    let dispatcher = swage_gateway::tool::ToolDispatcher::allow_all(
        swage_gateway::tool::browser::browser_tools(Arc::clone(&browser_host)),
    );
    let (router, addr) =
        start_browser_tool_server(provider, dispatcher, Arc::clone(&storage), temp.path()).await;

    let (_, body) = http(&router, "POST", "/api/conversations").await;
    let conv_id = body["id"].as_str().expect("conversation id").to_string();
    let (mut ws_write, mut ws_read) = connect_ws(addr, &conv_id).await;
    ws_send(
        &mut ws_write,
        json!({"type": "subscribe", "channels": ["tools"]}),
    )
    .await;
    ws_send(
        &mut ws_write,
        json!({"type": "send_message", "content": "Use the browser"}),
    )
    .await;

    tx1.send(StreamChunk::ToolUse {
        call_id: "browser-nav".into(),
        name: "browser_navigate".into(),
        arguments: json!({"url": fixture.url("/login")}),
    })
    .await
    .expect("send navigate chunk");
    tx1.send(StreamChunk::Done {
        finish_reason: None,
    })
    .await
    .expect("finish navigate round");
    drop(tx1);

    let navigate_end = ws_recv_until(&mut ws_read, |event| {
        event["type"] == "tool_end" && event["tool"] == "browser_navigate"
    })
    .await;
    let navigate_text = navigate_end["result"]["output"]
        .as_str()
        .expect("navigate output");
    let password_ref = extract_ref_for_name(navigate_text, "Password")
        .expect("password ref in navigate tool output");

    tx2.send(StreamChunk::ToolUse {
        call_id: "browser-secret-type".into(),
        name: "browser_type".into(),
        arguments: json!({"ref": password_ref, "text": secret.reference, "replace": true}),
    })
    .await
    .expect("send type chunk");
    tx2.send(StreamChunk::Done {
        finish_reason: None,
    })
    .await
    .expect("finish type round");
    drop(tx2);

    let approval = ws_recv_until(&mut ws_read, |event| event["type"] == "approval_required").await;
    let approval_id = approval["approval_id"].as_str().expect("approval id");
    ws_send(
        &mut ws_write,
        json!({
            "type": "approval_decision",
            "approval_id": approval_id,
            "decision": "approved"
        }),
    )
    .await;

    let type_end = ws_recv_until(&mut ws_read, |event| {
        event["type"] == "tool_end" && event["tool"] == "browser_type"
    })
    .await;
    assert_no_secret("browser_type tool_end event", &type_end.to_string());

    tx3.send(StreamChunk::Delta {
        content: "Done".into(),
    })
    .await
    .expect("send final text");
    tx3.send(StreamChunk::Done {
        finish_reason: Some(FinishReason::Stop),
    })
    .await
    .expect("finish final round");
    drop(tx3);
    let turn_complete = ws_recv_until(&mut ws_read, |event| event["type"] == "turn_complete").await;
    let turn_id = uuid::Uuid::parse_str(turn_complete["turn_id"].as_str().expect("turn id"))
        .expect("turn uuid");

    let calls = storage
        .list_tool_calls_by_turn(turn_id)
        .await
        .expect("tool call rows");
    assert_eq!(
        calls.len(),
        2,
        "expected navigate and type calls: {calls:#?}"
    );
    let type_call = calls
        .iter()
        .find(|call| call.tool == "browser_type")
        .expect("browser_type call row");
    assert!(
        type_call.arguments.contains("secret://"),
        "tool arguments should preserve the secret reference"
    );
    assert_no_secret(
        "browser_type tool_calls.result",
        type_call.result.as_deref().unwrap_or(""),
    );

    let audits = storage.list_audit_events().await.expect("audit rows");
    assert!(
        audits
            .iter()
            .any(|event| event.entity_kind == "browser" && event.event_kind == "browser_type"),
        "expected browser_type audit event: {audits:#?}"
    );
    for audit in &audits {
        assert_no_secret(
            "audit row",
            &format!("{} {}", audit.summary, audit.metadata),
        );
    }

    let type_result = type_call.result.as_deref().expect("browser_type result");
    let parsed_type_result = serde_json::from_str::<Value>(type_result).expect("type result JSON");
    assert!(
        parsed_type_result["screenshot"].is_null(),
        "browser_type must not capture post-type screenshots under the default actions policy: {type_result}"
    );
    let snapshot_path = parsed_type_result["full_snapshot"]
        .as_str()
        .map(ToString::to_string)
        .expect("browser_type should spill full snapshot artifact");
    let artifact_path = find_artifact_under(temp.path(), &snapshot_path).unwrap_or_else(|| {
        panic!(
            "snapshot artifact path did not exist under test temp; relative path: {snapshot_path}; result: {type_result}"
        )
    });
    let artifact = tokio::fs::read_to_string(&artifact_path)
        .await
        .expect("read spilled snapshot artifact");
    assert_no_secret("snapshot artifact", &artifact);

    browser_host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ax_typed_field_stays_masked_when_snapshot_falls_back_to_dom() {
    let _browser_guard = BROWSER_TEST_LOCK.lock().await;
    let Some(binary) = browser_binary_or_skip() else {
        return;
    };
    let fixture = FixtureServer::spawn().await;
    let temp = TempDir::new().expect("tempdir");
    let host = BrowserHost::new(browser_config(&binary, temp.path(), fixture.host()));

    let first = tokio::time::timeout(
        std::time::Duration::from_secs(15),
        host.navigate("mask-session", &fixture.url("/fallback-mask")),
    )
    .await
    .expect("navigate fallback mask page timed out")
    .expect("navigate fallback mask page");
    let token = first
        .refs
        .values()
        .find(|entry| entry.name == "Token")
        .expect("AX token ref");
    assert_ne!(
        token.backend_node_id,
        BackendNodeId::new(0),
        "test must type through an AX-backed ref before forcing DOM fallback:\n{}",
        first.output
    );

    host.type_text(
        "mask-session",
        &token.reference,
        "dom-fallback-secret",
        true,
        false,
    )
    .await
    .expect("type token");
    let fallback = host
        .snapshot("mask-session")
        .await
        .expect("fallback snapshot");
    assert!(
        fallback
            .refs
            .values()
            .all(|entry| entry.backend_node_id == BackendNodeId::new(0)),
        "expected forced DOM fallback refs after typing, got:\n{}",
        fallback.output
    );
    assert!(
        !fallback.output.contains("dom-fallback-secret"),
        "AX-masked field leaked after DOM fallback:\n{}",
        fallback.output
    );
    assert!(
        fallback.output.contains("value=\"[masked]\""),
        "DOM fallback did not keep the AX-applied mask:\n{}",
        fallback.output
    );
    host.shutdown().await;
}

#[test]
fn browser_permission_bundles_split_read_from_actions() {
    let defaults = ToolPermissionConfig::default();
    let read_only = PermissionConfig {
        allow: vec!["browsing".to_string()],
        deny: Vec::new(),
        bundles: defaults.bundles,
        require_approval: Vec::new(),
    };
    assert!(check_permission(&read_only, "browser_snapshot").is_ok());
    assert!(check_permission(&read_only, "browser_navigate").is_ok());
    assert!(check_permission(&read_only, "browser_click").is_err());
    assert!(check_permission(&read_only, "browser_type").is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "hand-run live smoke against the public web; never run in CI"]
async fn live_smoke_public_site() {
    let _browser_guard = BROWSER_TEST_LOCK.lock().await;
    let Some(binary) = browser_binary_or_skip() else {
        return;
    };
    let temp = TempDir::new().expect("tempdir");
    let host = BrowserHost::new(browser_config(
        &binary,
        temp.path(),
        "example.com".to_string(),
    ));
    let snapshot = host
        .navigate("live-smoke", "https://example.com/")
        .await
        .expect("navigate example.com");
    println!("{}", snapshot.output);
    assert!(snapshot.output.contains("Example Domain"));
    host.shutdown().await;
}

fn browser_config(binary: &str, root: &std::path::Path, host: String) -> BrowserConfig {
    browser_config_with_screenshots(binary, root, host, BrowserScreenshotPolicy::Off)
}

fn browser_config_with_screenshots(
    binary: &str,
    root: &std::path::Path,
    host: String,
    screenshots: BrowserScreenshotPolicy,
) -> BrowserConfig {
    let tool_config = BrowserToolConfig {
        binary_path: Some(binary.to_string()),
        page_idle_secs: 30,
        process_idle_secs: 30,
        max_snapshot_bytes: 40_000,
        screenshots,
        allow_hosts: vec![host],
    };
    BrowserConfig {
        binary_path: tool_config.binary_path,
        profile_root: root.join("profiles"),
        page_idle: std::time::Duration::from_secs(tool_config.page_idle_secs),
        process_idle: std::time::Duration::from_secs(tool_config.process_idle_secs),
        max_snapshot_bytes: tool_config.max_snapshot_bytes,
        screenshots: tool_config.screenshots,
        allow_hosts: tool_config.allow_hosts,
    }
}

fn browser_binary_or_skip() -> Option<String> {
    if let Some(candidate) = [
        std::env::var("SWAGE_BROWSER_BIN").ok(),
        which("chromium"),
        which("google-chrome-stable"),
        which("chromium-browser"),
    ]
    .into_iter()
    .flatten()
    .next()
    {
        return Some(candidate);
    }
    assert!(
        std::env::var("SWAGE_BROWSER_TESTS").as_deref() != Ok("required"),
        "SWAGE_BROWSER_TESTS=required but no Chromium binary was found"
    );
    eprintln!("skipping browser journey: no Chromium binary found");
    None
}

fn which(binary: &str) -> Option<String> {
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|path| path.join(binary))
            .find(|path| path.is_file())
            .map(|path| path.display().to_string())
    })
}

struct ChannelProvider {
    receivers: std::sync::Mutex<VecDeque<tokio::sync::mpsc::Receiver<StreamChunk>>>,
}

impl ChannelProvider {
    fn new(receivers: Vec<tokio::sync::mpsc::Receiver<StreamChunk>>) -> Self {
        Self {
            receivers: std::sync::Mutex::new(receivers.into()),
        }
    }
}

#[async_trait]
impl LlmProvider for ChannelProvider {
    fn model_id(&self) -> &str {
        "browser-channel-model"
    }

    async fn complete(
        &self,
        _request: CompletionRequest,
    ) -> Result<CompletionResponse, ProviderError> {
        unimplemented!("browser integration uses streaming")
    }

    async fn complete_with_tools(
        &self,
        _request: ToolCompletionRequest,
    ) -> Result<ToolCompletionResponse, ProviderError> {
        unimplemented!("browser integration uses streaming")
    }

    fn stream(
        &self,
        _request: CompletionRequest,
    ) -> Pin<Box<dyn futures::Stream<Item = StreamChunk> + Send>> {
        let rx = self
            .receivers
            .lock()
            .expect("provider receivers")
            .pop_front()
            .expect("no stream receiver configured");
        Box::pin(futures::stream::unfold(rx, |mut rx| async {
            rx.recv().await.map(|chunk| (chunk, rx))
        }))
    }
}

async fn start_browser_tool_server(
    provider: impl LlmProvider + 'static,
    dispatcher: swage_gateway::tool::ToolDispatcher,
    storage: Arc<SqliteStorage>,
    _workspace_root: &std::path::Path,
) -> (Router, SocketAddr) {
    let mut config = swage_config::ResolvedConfig::default();
    // Approval prompting defaults off; this attended-session journey asserts
    // the approval flow, so opt the typed-secret tool in explicitly.
    config.tools.permissions.require_approval =
        vec![swage_config::model::RequireApprovalEntry::Simple(
            "browser_type".to_string(),
        )];
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>,
        Arc::clone(&storage) as Arc<dyn SpaceStore>,
        Arc::new(provider),
        config,
        dispatcher,
        None,
    );
    let router = build_router(state);
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind test gateway");
    let addr = listener.local_addr().expect("gateway address");
    let serve_router = router.clone();
    tokio::spawn(async move {
        axum::serve(listener, serve_router)
            .await
            .expect("serve test gateway");
    });
    (router, addr)
}

async fn http(router: &Router, method: &str, uri: &str) -> (StatusCode, Value) {
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .body(Body::empty())
                .expect("request body"),
        )
        .await
        .expect("HTTP request");
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("response bytes");
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, body)
}

type WsSink = futures::stream::SplitSink<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    tungstenite::Message,
>;
type WsStream = futures::stream::SplitStream<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
>;

async fn connect_ws(addr: SocketAddr, conversation_id: &str) -> (WsSink, WsStream) {
    let url = format!("ws://{addr}/api/conversations/{conversation_id}/ws");
    let (ws, _) = tokio_tungstenite::connect_async(&url)
        .await
        .expect("connect WebSocket");
    ws.split()
}

async fn ws_send(sink: &mut WsSink, payload: Value) {
    sink.send(tungstenite::Message::Text(payload.to_string().into()))
        .await
        .expect("send WebSocket event");
}

async fn ws_recv(stream: &mut WsStream) -> Value {
    loop {
        match stream.next().await {
            Some(Ok(tungstenite::Message::Text(text))) => {
                return serde_json::from_str(&text).expect("valid WebSocket JSON");
            }
            Some(Ok(tungstenite::Message::Close(_))) => panic!("WebSocket closed"),
            Some(Err(error)) => panic!("WebSocket error: {error}"),
            None => panic!("WebSocket ended"),
            _ => continue,
        }
    }
}

async fn ws_recv_until(stream: &mut WsStream, predicate: impl Fn(&Value) -> bool) -> Value {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
    let mut seen = Vec::new();
    loop {
        let event = tokio::time::timeout_at(deadline, ws_recv(stream))
            .await
            .unwrap_or_else(|_| panic!("timed out waiting for WebSocket event; seen: {seen:#?}"));
        if predicate(&event) {
            return event;
        }
        seen.push(event);
    }
}

fn extract_ref_for_name(snapshot_text: &str, name: &str) -> Option<String> {
    snapshot_text.lines().find_map(|line| {
        if !line.contains(&format!("\"{name}\"")) {
            return None;
        }
        line.split_whitespace()
            .find_map(|part| part.strip_prefix("ref=").map(ToString::to_string))
    })
}

fn assert_no_secret(surface: &str, text: &str) {
    assert!(
        !text.contains("resolved-fixture-password"),
        "{surface} leaked resolved secret:\n{text}"
    );
}

fn find_artifact_under(root: &std::path::Path, relative: &str) -> Option<std::path::PathBuf> {
    let suffix = std::path::Path::new(relative);
    let mut stack = vec![root.to_path_buf()];
    while let Some(path) = stack.pop() {
        if path.ends_with(suffix) && path.is_file() {
            return Some(path);
        }
        let Ok(entries) = std::fs::read_dir(&path) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.ends_with(suffix) {
                return Some(path);
            }
        }
    }
    None
}

fn ax_fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("browser")
        .join("ax")
}

fn maybe_record_ax_fixture(
    name: &str,
    nodes: &[chromiumoxide::cdp::browser_protocol::accessibility::AxNode],
    rendered: &str,
) {
    if std::env::var("SWAGE_RECORD_AX_FIXTURES").as_deref() != Ok("1") {
        return;
    }
    let fixture_dir = ax_fixture_dir();
    std::fs::create_dir_all(&fixture_dir).unwrap_or_else(|error| {
        panic!(
            "failed to create AX fixture directory '{}': {error}",
            fixture_dir.display()
        )
    });
    let json_path = fixture_dir.join(format!("{name}.json"));
    let rendered_path = fixture_dir.join(format!("{name}.rendered"));
    let json = serde_json::to_string_pretty(nodes).expect("serialise AX fixture");
    let stable_rendered = rendered.replacen(
        rendered.lines().next().unwrap_or_default(),
        &format!("url: https://fixture.local/{name}"),
        1,
    );
    std::fs::write(&json_path, format!("{json}\n")).unwrap_or_else(|error| {
        panic!(
            "failed to write AX fixture '{}': {error}",
            json_path.display()
        )
    });
    std::fs::write(&rendered_path, format!("{stable_rendered}\n")).unwrap_or_else(|error| {
        panic!(
            "failed to write AX rendered golden '{}': {error}",
            rendered_path.display()
        )
    });
}

fn read_ax_nodes(path: &Path) -> Vec<chromiumoxide::cdp::browser_protocol::accessibility::AxNode> {
    let json = std::fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("failed to read AX fixture '{}': {error}", path.display()));
    serde_json::from_str(&json)
        .unwrap_or_else(|error| panic!("failed to parse AX fixture '{}': {error}", path.display()))
}

fn reserve_probe_port() -> u16 {
    std::net::TcpListener::bind(("127.0.0.1", 0))
        .and_then(|listener| listener.local_addr())
        .expect("reserve probe port")
        .port()
}

async fn spawn_probe_browser(
    binary: &str,
    root: &std::path::Path,
    port: u16,
) -> (Browser, tokio::task::JoinHandle<()>, Child) {
    let profile = root.join("profiles").join("probe");
    tokio::fs::create_dir_all(&profile)
        .await
        .expect("probe profile");
    let child = tokio::process::Command::new(binary)
        .arg("--headless=new")
        .arg(format!("--user-data-dir={}", profile.display()))
        .arg("--disable-gpu")
        .arg("--password-store=basic")
        .arg("--force-renderer-accessibility")
        .arg("--no-first-run")
        .arg("--no-default-browser-check")
        .arg("--remote-debugging-address=127.0.0.1")
        .arg(format!("--remote-debugging-port={port}"))
        .arg("about:blank")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn probe Chromium");
    let endpoint = format!("http://127.0.0.1:{port}");
    wait_for_probe_devtools(&endpoint).await;
    let (browser, mut events) = Browser::connect(endpoint).await.expect("connect probe CDP");
    let handler = tokio::spawn(async move {
        while let Some(event) = events.next().await {
            if let Err(error) = event {
                eprintln!("probe CDP event failed: {error}");
            }
        }
    });
    (browser, handler, child)
}

async fn wait_for_probe_devtools(endpoint: &str) {
    let client = reqwest::Client::new();
    let version_url = format!("{endpoint}/json/version");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        if let Ok(response) = client.get(&version_url).send().await
            && response.status().is_success()
        {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "probe Chromium did not open DevTools endpoint"
        );
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}

struct FixtureServer {
    addr: SocketAddr,
    shutdown: CancellationToken,
    saw_login: Arc<AtomicBool>,
}

impl FixtureServer {
    async fn spawn() -> Self {
        let saw_login = Arc::new(AtomicBool::new(false));
        let app = Router::new()
            .route("/login", get(login_page).post(login_post))
            .route("/fallback-mask", get(fallback_mask_page))
            .route("/single-action", get(single_action_page))
            .route("/mutate", get(mutate_page))
            .route("/set-cookie", get(set_cookie))
            .route("/signed-in", get(signed_in))
            .route("/whoami", get(whoami))
            .with_state(Arc::clone(&saw_login));
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("bind fixture");
        let addr = listener.local_addr().expect("fixture address");
        let shutdown = CancellationToken::new();
        let shutdown_for_task = shutdown.clone();
        tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    shutdown_for_task.cancelled().await;
                })
                .await
                .expect("fixture server");
        });
        Self {
            addr,
            shutdown,
            saw_login,
        }
    }

    fn host(&self) -> String {
        self.addr.ip().to_string()
    }

    fn url(&self, path: &str) -> String {
        format!("http://{}{}", self.addr, path)
    }
}

impl Drop for FixtureServer {
    fn drop(&mut self) {
        self.shutdown.cancel();
    }
}

async fn login_page() -> Html<&'static str> {
    Html(
        r#"<!doctype html>
        <title>Fixture Login</title>
        <h1>Sign in</h1>
        <form method="post" action="/login">
            <label>Username <input name="username" autocomplete="username"></label>
            <label>Password <input name="password" type="password" autocomplete="current-password"></label>
            <a href="/set-cookie">Sign in</a>
        </form>"#,
    )
}

async fn login_post(State(saw_login): State<Arc<AtomicBool>>) -> impl IntoResponse {
    saw_login.store(true, Ordering::SeqCst);
    (
        StatusCode::SEE_OTHER,
        [
            (
                "set-cookie",
                "swage_fixture_user=ben; Path=/; Max-Age=3600; SameSite=Lax",
            ),
            ("location", "/signed-in"),
        ],
        Html(""),
    )
}

async fn fallback_mask_page() -> Html<&'static str> {
    Html(
        r#"<!doctype html>
        <title>Fallback Mask</title>
        <main id="ax-region">
            <label>Token <input name="token" autocomplete="off"></label>
        </main>
        <script>
        document.querySelector('input[name="token"]').addEventListener('input', () => {
            document.querySelector('#ax-region').setAttribute('aria-hidden', 'true');
            document.querySelector('#ax-region').style.display = 'none';
        });
        </script>"#,
    )
}

async fn single_action_page() -> Html<&'static str> {
    Html(
        r#"<!doctype html>
        <title>Single Action</title>
        <h1>One action</h1>
        <button>Only current action</button>"#,
    )
}

async fn mutate_page() -> Html<&'static str> {
    Html(
        r#"<!doctype html>
        <title>Mutating Page</title>
        <main aria-hidden="true">
            <h1>Mutating page</h1>
            <button id="doomed">Remove me</button>
        </main>
        <script>
        setTimeout(() => document.querySelector('#doomed')?.remove(), 1500);
        </script>"#,
    )
}

async fn signed_in() -> Html<&'static str> {
    Html("<title>Signed in</title><h1>Signed in</h1>")
}

async fn set_cookie(State(saw_login): State<Arc<AtomicBool>>) -> impl IntoResponse {
    saw_login.store(true, Ordering::SeqCst);
    (
        StatusCode::SEE_OTHER,
        [
            (
                "set-cookie",
                "swage_fixture_user=ben; Path=/; Max-Age=3600; SameSite=Lax",
            ),
            ("location", "/signed-in"),
        ],
        Html(""),
    )
}

async fn whoami(headers: HeaderMap) -> impl IntoResponse {
    let cookie = headers
        .get("cookie")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    if cookie.contains("swage_fixture_user=ben") {
        Html("<title>Whoami</title><h1>Signed in as ben</h1>").into_response()
    } else {
        Redirect::to("/login").into_response()
    }
}

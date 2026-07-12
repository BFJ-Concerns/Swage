#![cfg(feature = "fake-app-server")]

use std::{path::PathBuf, time::Duration};

use swage_codex::{AppServerConfig, AppServerSupervisor};

#[tokio::test]
async fn fake_server_handshakes_and_serves_requests() {
    let fake = PathBuf::from(env!("CARGO_BIN_EXE_fake-app-server"));
    let state = tempfile::tempdir().expect("temporary state");
    let supervisor = AppServerSupervisor::new(AppServerConfig {
        command: fake.display().to_string(),
        state_dir: state.path().to_path_buf(),
        request_timeout: Duration::from_secs(2),
    });
    let handle = supervisor.ensure_running().await.expect("start fake");
    assert_eq!(
        handle.user_agent.as_deref(),
        Some("swage-fake-app-server/1")
    );
    let account = handle
        .client
        .request("account/read", serde_json::json!({}))
        .await
        .expect("account response");
    assert_eq!(account["account"]["email"], "fake@example.test");
    supervisor.shutdown().await;
}

#[tokio::test]
async fn fake_server_pins_thread_start_and_stream_order() {
    let fake = PathBuf::from(env!("CARGO_BIN_EXE_fake-app-server"));
    let state = tempfile::tempdir().expect("temporary state");
    let supervisor = AppServerSupervisor::new(AppServerConfig {
        command: fake.display().to_string(),
        state_dir: state.path().to_path_buf(),
        request_timeout: Duration::from_secs(2),
    });
    let client = supervisor.client().await.expect("client");
    let mut notifications = client.subscribe();
    let thread = client
        .request(
            "thread/start",
            serde_json::json!({
                "model":"fake-codex",
                "sandbox":"read-only",
                "approvalPolicy":"on-request",
                "cwd":"/tmp",
                "developerInstructions":"test context",
                "dynamicTools":[],
                "ephemeral":false
            }),
        )
        .await
        .expect("thread start");
    assert_eq!(thread["thread"]["id"], "fake-thread");
    client
        .request(
            "turn/start",
            serde_json::json!({
                "threadId":"fake-thread",
                "input":[{"type":"text","text":"hello"}]
            }),
        )
        .await
        .expect("turn start");
    let mut methods = Vec::new();
    while methods.last().map(String::as_str) != Some("turn/completed") {
        methods.push(notifications.recv().await.expect("notification").method);
    }
    assert_eq!(methods.first().map(String::as_str), Some("turn/started"));
    assert_eq!(methods.last().map(String::as_str), Some("turn/completed"));
    supervisor.shutdown().await;
}

#[tokio::test]
#[ignore = "requires the operator-installed Codex CLI and its managed account"]
async fn live_cli_initialises_and_reads_account_and_models() {
    let home = std::env::var_os("HOME").expect("HOME");
    let supervisor =
        AppServerSupervisor::new(AppServerConfig::codex(PathBuf::from(home).join(".codex")));
    let client = supervisor.client().await.expect("live Codex client");
    client
        .request("account/read", serde_json::json!({"refreshToken":true}))
        .await
        .expect("account/read");
    let models = client
        .request("model/list", serde_json::json!({}))
        .await
        .expect("model/list");
    assert!(
        models
            .get("data")
            .and_then(serde_json::Value::as_array)
            .is_some()
    );
    supervisor.shutdown().await;
}

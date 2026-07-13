#![cfg(feature = "fake-app-server")]

use std::{path::PathBuf, time::Duration};

use swage_codex::{
    AppServerClient, AppServerConfig, AppServerSupervisor, ClientError, SupervisorError,
};

fn scripted_client(
    scenario: serde_json::Value,
) -> (
    AppServerClient,
    tokio::task::JoinHandle<Result<Vec<serde_json::Value>, String>>,
) {
    let (client_side, server_side) = tokio::io::duplex(16 * 1024);
    let (client_read, client_write) = tokio::io::split(client_side);
    let (server_read, server_write) = tokio::io::split(server_side);
    let client = AppServerClient::new(client_write, client_read, Duration::from_millis(250));
    let server = tokio::spawn(async move {
        swage_codex::fake::run_scenario(
            &scenario,
            tokio::io::BufReader::new(server_read),
            server_write,
        )
        .await
    });
    (client, server)
}

#[tokio::test]
async fn malformed_scenario_frame_closes_the_pending_request_cleanly() {
    let (client, server) = scripted_client(serde_json::json!({"steps":[{
        "receive":{"method":"account/read"},
        "send":[{"raw":"this is not JSON"}],
        "close":true
    }]}));

    assert!(matches!(
        client.request("account/read", serde_json::json!({})).await,
        Err(ClientError::Closed)
    ));
    server.await.unwrap().unwrap();
}

#[tokio::test]
async fn mid_turn_server_death_closes_the_notification_stream() {
    let (client, server) = scripted_client(serde_json::json!({"steps":[{
        "receive":{"method":"turn/start"},
        "send":[
            {"jsonrpc":"2.0","id":"$request_id","result":{"turn":{"id":"fake-turn"}}},
            {"jsonrpc":"2.0","method":"turn/started","params":{"turn":{"id":"fake-turn","status":"inProgress"}}}
        ],
        "close":true
    }]}));
    let mut notifications = client.subscribe();

    client
        .request("turn/start", serde_json::json!({}))
        .await
        .expect("turn starts before the fake dies");
    assert_eq!(notifications.recv().await.unwrap().method, "turn/started");
    client.closed().await;
    server.await.unwrap().unwrap();
}

#[tokio::test]
async fn failed_start_is_backed_off_before_another_spawn() {
    let state = tempfile::tempdir().expect("temporary state");
    let supervisor = AppServerSupervisor::new(AppServerConfig {
        command: "/bin/false".to_string(),
        state_dir: state.path().to_path_buf(),
        request_timeout: Duration::from_millis(250),
    });

    assert!(supervisor.ensure_running().await.is_err());
    assert!(matches!(
        supervisor.ensure_running().await,
        Err(SupervisorError::Backoff { retry_after_ms }) if retry_after_ms <= 250
    ));
}

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

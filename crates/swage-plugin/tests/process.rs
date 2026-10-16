//! The brokered process's lifecycle and door, against the fixture process
//! (`src/bin/swage-plugin-fixture.rs`).
#![allow(clippy::unwrap_used)]

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde_json::{Value, json};
use swage_plugin::{
    HostCallFailure, HostGrant, InvocationContext, PlainLauncher, PluginError, PluginHost,
    ProcessObserver, ProcessPlugin, ProcessSpec, SilentObserver,
};
use swage_protocol::plugin::PluginProcessStopReason;

fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_swage-plugin-fixture"))
}

fn spec(grants: Vec<HostGrant>, args: Vec<String>, idle: Duration) -> ProcessSpec {
    ProcessSpec {
        plugin: "fixture".into(),
        command: fixture(),
        args,
        idle,
        grants,
        tools: vec!["echo".into(), "count".into(), "call".into(), "log".into()],
    }
}

fn context(session: &str) -> InvocationContext {
    InvocationContext {
        session_id: session.into(),
        agent_id: "agent-1".into(),
        call_id: Some("call-1".into()),
        working_directory: "/work".into(),
    }
}

/// A host that answers every tool call with what it was asked and records
/// the calls and log lines.
#[derive(Default)]
struct RecordingHost {
    calls: Mutex<Vec<(String, Value)>>,
    logged: Mutex<Vec<String>>,
}

#[async_trait]
impl PluginHost for RecordingHost {
    async fn call_tool(&self, name: &str, arguments: Value) -> Result<Value, HostCallFailure> {
        self.calls
            .lock()
            .unwrap()
            .push((name.to_owned(), arguments.clone()));
        if name == "refused" {
            return Err(HostCallFailure {
                name: "PermissionDenied".into(),
                message: "the session does not grant 'refused'".into(),
            });
        }
        Ok(json!({ "ran": name, "with": arguments }))
    }

    fn log(&self, message: &str) {
        self.logged.lock().unwrap().push(message.to_owned());
    }
}

/// Records every lifecycle event, in order.
#[derive(Default)]
struct Events(Mutex<Vec<String>>);

impl ProcessObserver for Events {
    fn started(&self, plugin: &str, session: &str, _pid: u32) {
        self.0
            .lock()
            .unwrap()
            .push(format!("started {plugin} {session}"));
    }
    fn stopped(&self, plugin: &str, session: &str, reason: PluginProcessStopReason) {
        self.0
            .lock()
            .unwrap()
            .push(format!("stopped {plugin} {session} {reason:?}"));
    }
    fn exited(&self, plugin: &str, session: &str, detail: &str) {
        self.0
            .lock()
            .unwrap()
            .push(format!("exited {plugin} {session} {detail}"));
    }
}

async fn invoke(
    plugin: &Arc<ProcessPlugin>,
    session: &str,
    tool: &str,
    params: Value,
    host: &Arc<RecordingHost>,
) -> Result<Value, PluginError> {
    plugin
        .invoke_tool(
            tool,
            &params,
            &context(session),
            Arc::clone(host) as Arc<dyn PluginHost>,
            &PlainLauncher,
        )
        .await
}

#[tokio::test(flavor = "multi_thread")]
async fn first_use_starts_a_process_per_session_that_keeps_its_state() {
    let events = Arc::new(Events::default());
    let plugin = ProcessPlugin::new(
        spec(vec![], vec![], Duration::from_secs(60)),
        Arc::clone(&events) as Arc<dyn ProcessObserver>,
    );
    let host = Arc::new(RecordingHost::default());
    assert!(plugin.serving_sessions().is_empty());

    let first = invoke(&plugin, "session-a", "count", json!({}), &host)
        .await
        .expect("serves");
    let second = invoke(&plugin, "session-a", "count", json!({}), &host)
        .await
        .expect("serves");
    let other = invoke(&plugin, "session-b", "count", json!({}), &host)
        .await
        .expect("serves");
    assert_eq!(first["count"], 1);
    assert_eq!(second["count"], 2, "the session's process kept its state");
    assert_eq!(
        other["count"], 1,
        "the other session got a process of its own"
    );
    assert_ne!(first["pid"], other["pid"]);
    assert_eq!(second["pid"], first["pid"]);
    assert_eq!(
        plugin.serving_sessions(),
        vec!["session-a".to_owned(), "session-b".to_owned()]
    );
    let echoed = invoke(&plugin, "session-a", "echo", json!({ "text": "hi" }), &host)
        .await
        .expect("serves");
    assert_eq!(echoed, json!({ "text": "hi" }));
    assert_eq!(
        *events.0.lock().unwrap(),
        vec!["started fixture session-a", "started fixture session-b"]
    );
    plugin
        .stop_all(PluginProcessStopReason::GatewayStopping)
        .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_host_call_from_the_process_is_answered_under_the_plugin_s_grants() {
    let granted = ProcessPlugin::new(
        spec(vec![HostGrant::Tools], vec![], Duration::from_secs(60)),
        Arc::new(SilentObserver),
    );
    let host = Arc::new(RecordingHost::default());
    let ran = invoke(
        &granted,
        "s",
        "call",
        json!({ "tool": "shell", "arguments": { "command": "ls" } }),
        &host,
    )
    .await
    .expect("serves");
    assert_eq!(
        ran,
        json!({ "ran": { "ran": "shell", "with": { "command": "ls" } } })
    );
    let refused = invoke(&granted, "s", "call", json!({ "tool": "refused" }), &host)
        .await
        .expect("serves");
    assert_eq!(
        refused["refused"],
        "PermissionDenied: the session does not grant 'refused'"
    );
    let logged = invoke(&granted, "s", "log", json!({ "message": "note" }), &host)
        .await
        .expect("serves");
    assert_eq!(logged, json!({ "logged": true }));
    assert_eq!(*host.logged.lock().unwrap(), vec!["note"]);
    granted
        .stop_all(PluginProcessStopReason::GatewayStopping)
        .await;

    let ungranted = ProcessPlugin::new(
        spec(vec![], vec![], Duration::from_secs(60)),
        Arc::new(SilentObserver),
    );
    let host = Arc::new(RecordingHost::default());
    let refused = invoke(&ungranted, "s", "call", json!({ "tool": "shell" }), &host)
        .await
        .expect("serves");
    assert!(
        refused["refused"]
            .as_str()
            .unwrap()
            .starts_with("HostCapabilityError:"),
        "{refused}"
    );
    assert!(
        host.calls.lock().unwrap().is_empty(),
        "nothing reached the host"
    );
    ungranted
        .stop_all(PluginProcessStopReason::GatewayStopping)
        .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn session_close_idle_and_shutdown_each_stop_the_process_with_the_reason() {
    let dir = tempfile::tempdir().unwrap();
    let stop_file = dir.path().join("stop-reason");
    let events = Arc::new(Events::default());
    let plugin = ProcessPlugin::new(
        spec(
            vec![],
            vec!["--stop-file".into(), stop_file.display().to_string()],
            Duration::from_millis(200),
        ),
        Arc::clone(&events) as Arc<dyn ProcessObserver>,
    );
    let host = Arc::new(RecordingHost::default());

    invoke(&plugin, "closing", "echo", json!({}), &host)
        .await
        .expect("serves");
    assert!(plugin.stop_session("closing").await);
    assert!(!plugin.stop_session("closing").await, "stopped once");
    assert_eq!(
        std::fs::read_to_string(&stop_file).unwrap(),
        "SessionClosed"
    );
    assert!(plugin.serving_sessions().is_empty());

    invoke(&plugin, "idling", "echo", json!({}), &host)
        .await
        .expect("serves");
    assert!(
        plugin.sweep_idle(Instant::now()).await.is_empty(),
        "a process just used is not idle"
    );
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert_eq!(plugin.sweep_idle(Instant::now()).await, vec!["idling"]);
    assert_eq!(std::fs::read_to_string(&stop_file).unwrap(), "Idle");

    invoke(&plugin, "stopping-1", "echo", json!({}), &host)
        .await
        .expect("serves");
    invoke(&plugin, "stopping-2", "echo", json!({}), &host)
        .await
        .expect("serves");
    let mut stopped = plugin
        .stop_all(PluginProcessStopReason::GatewayStopping)
        .await;
    stopped.sort();
    assert_eq!(stopped, vec!["stopping-1", "stopping-2"]);
    assert_eq!(
        std::fs::read_to_string(&stop_file).unwrap(),
        "GatewayStopping"
    );

    let events = events.0.lock().unwrap().clone();
    assert!(
        events.contains(&"stopped fixture closing SessionClosed".to_owned()),
        "{events:?}"
    );
    assert!(
        events.contains(&"stopped fixture idling Idle".to_owned()),
        "{events:?}"
    );
    assert!(
        events.contains(&"stopped fixture stopping-1 GatewayStopping".to_owned()),
        "{events:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_process_that_dies_mid_call_fails_the_call_and_the_next_call_starts_afresh() {
    let events = Arc::new(Events::default());
    let plugin = ProcessPlugin::new(
        spec(vec![], vec![], Duration::from_secs(60)),
        Arc::clone(&events) as Arc<dyn ProcessObserver>,
    );
    let host = Arc::new(RecordingHost::default());
    let first = invoke(&plugin, "s", "count", json!({}), &host)
        .await
        .expect("serves");
    assert_eq!(first["count"], 1);
    let died = invoke(&plugin, "s", "exit", json!({}), &host)
        .await
        .expect_err("the process ended mid-call");
    assert!(matches!(died, PluginError::ProcessExited(_)), "{died}");
    let fresh = invoke(&plugin, "s", "count", json!({}), &host)
        .await
        .expect("a fresh process serves");
    assert_eq!(fresh["count"], 1, "the state went with the process");
    assert_ne!(fresh["pid"], first["pid"]);
    let events = events.0.lock().unwrap().clone();
    assert!(
        events
            .iter()
            .any(|event| event.starts_with("exited fixture s ")),
        "{events:?}"
    );
    plugin
        .stop_all(PluginProcessStopReason::GatewayStopping)
        .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_process_that_does_not_serve_a_declared_tool_fails_the_start() {
    let mut spec = spec(vec![], vec![], Duration::from_secs(60));
    spec.tools.push("absent".into());
    let plugin = ProcessPlugin::new(spec, Arc::new(SilentObserver));
    let host = Arc::new(RecordingHost::default());
    let error = invoke(&plugin, "s", "echo", json!({}), &host)
        .await
        .expect_err("refused");
    assert!(
        matches!(&error, PluginError::Script(message) if message.contains("'absent'")),
        "{error}"
    );
    assert!(plugin.serving_sessions().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_command_that_cannot_run_fails_the_start_with_the_reason() {
    let mut spec = spec(vec![], vec![], Duration::from_secs(60));
    spec.command = PathBuf::from("/definitely/not/a/plugin/process");
    let plugin = ProcessPlugin::new(spec, Arc::new(SilentObserver));
    let host = Arc::new(RecordingHost::default());
    let error = invoke(&plugin, "s", "echo", json!({}), &host)
        .await
        .expect_err("refused");
    assert!(matches!(error, PluginError::Process(_)), "{error}");
}

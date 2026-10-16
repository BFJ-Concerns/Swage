//! A plugin process for the kernel's tests, and the reference for writing
//! one: it connects to the socket named in `SWAGE_PLUGIN_SOCKET`, announces
//! its tools, answers each `invoke`, and makes host calls through the
//! gateway. Its tools: `echo` returns its params; `count` returns how many
//! times it has been called in this process (state a session keeps);
//! `call` runs the session tool named by `params.tool` through the host;
//! `log` writes `params.message` to the host's log; `hang` never answers;
//! `exit` ends the process mid-call. With `--stop-file <path>` it writes
//! the reason it was stopped for to that path before exiting.

use std::collections::HashMap;

use serde_json::{Value, json};
use swage_protocol::plugin::{
    PLUGIN_SOCKET_ENV, PluginHostReply, PluginProcessMessage, PluginProcessRequest,
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::{Mutex, mpsc, oneshot};

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let stop_file = {
        let mut args = std::env::args().skip(1);
        let mut stop_file = None;
        while let Some(arg) = args.next() {
            if arg == "--stop-file" {
                stop_file = args.next();
            }
        }
        stop_file
    };
    let socket = std::env::var(PLUGIN_SOCKET_ENV).expect("SWAGE_PLUGIN_SOCKET names the socket");
    let stream = UnixStream::connect(&socket)
        .await
        .expect("the gateway's socket accepts");
    let (read_half, mut write_half) = stream.into_split();
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<PluginProcessMessage>();
    tokio::spawn(async move {
        while let Some(message) = out_rx.recv().await {
            let mut line = serde_json::to_string(&message).expect("a message serialises");
            line.push('\n');
            if write_half.write_all(line.as_bytes()).await.is_err() {
                return;
            }
            let _ = write_half.flush().await;
        }
    });
    out_tx
        .send(PluginProcessMessage::Ready {
            tools: ["echo", "count", "call", "log", "hang", "exit"]
                .into_iter()
                .map(str::to_owned)
                .collect(),
        })
        .expect("the writer is up");

    let state = std::sync::Arc::new(Mutex::new(FixtureState::default()));
    let mut lines = BufReader::new(read_half).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let request: PluginProcessRequest = match serde_json::from_str(&line) {
            Ok(request) => request,
            Err(error) => {
                eprintln!("fixture: unreadable request: {error}");
                continue;
            }
        };
        match request {
            PluginProcessRequest::Invoke {
                id,
                tool,
                params,
                context: _,
            } => {
                let out_tx = out_tx.clone();
                let state = std::sync::Arc::clone(&state);
                tokio::spawn(async move {
                    let message = serve(&id, &tool, params, &out_tx, &state).await;
                    if let Some(message) = message {
                        let _ = out_tx.send(message);
                    }
                });
            }
            PluginProcessRequest::Reply { id, reply } => {
                if let Some(waiting) = state.lock().await.calls.remove(&id) {
                    let _ = waiting.send(reply);
                }
            }
            PluginProcessRequest::Stop { reason } => {
                if let Some(path) = &stop_file {
                    std::fs::write(path, format!("{reason:?}")).expect("the stop file is writable");
                }
                return;
            }
        }
    }
}

#[derive(Default)]
struct FixtureState {
    count: u64,
    calls: HashMap<String, oneshot::Sender<PluginHostReply>>,
}

/// A host call: send it, await the gateway's reply.
async fn host_call(
    invocation: &str,
    name: &str,
    payload: Value,
    out_tx: &mpsc::UnboundedSender<PluginProcessMessage>,
    state: &Mutex<FixtureState>,
) -> PluginHostReply {
    let id = format!("{invocation}:{name}:{}", std::process::id());
    let (reply_tx, reply_rx) = oneshot::channel();
    state.lock().await.calls.insert(id.clone(), reply_tx);
    let _ = out_tx.send(PluginProcessMessage::Call {
        id,
        invocation: invocation.to_owned(),
        name: name.to_owned(),
        payload,
    });
    reply_rx.await.unwrap_or(PluginHostReply {
        ok: false,
        value: None,
        error: None,
    })
}

async fn serve(
    id: &str,
    tool: &str,
    params: Value,
    out_tx: &mpsc::UnboundedSender<PluginProcessMessage>,
    state: &Mutex<FixtureState>,
) -> Option<PluginProcessMessage> {
    let result = |value: Value| PluginProcessMessage::Result {
        id: id.to_owned(),
        value,
    };
    match tool {
        "echo" => Some(result(params)),
        "count" => {
            let mut state = state.lock().await;
            state.count += 1;
            Some(result(
                json!({ "count": state.count, "pid": std::process::id() }),
            ))
        }
        "call" => {
            let name = params["tool"].as_str().unwrap_or_default().to_owned();
            let arguments = params
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            let reply = host_call(
                id,
                "callTool",
                json!({ "name": name, "arguments": arguments }),
                out_tx,
                state,
            )
            .await;
            Some(if reply.ok {
                result(json!({ "ran": reply.value }))
            } else {
                let error =
                    reply
                        .error
                        .unwrap_or_else(|| swage_protocol::plugin::PluginHostCallError {
                            name: "HostUnavailableError".into(),
                            message: "no reply".into(),
                        });
                result(json!({ "refused": format!("{}: {}", error.name, error.message) }))
            })
        }
        "log" => {
            let message = params["message"].as_str().unwrap_or_default().to_owned();
            let reply = host_call(id, "log", json!(message), out_tx, state).await;
            Some(result(json!({ "logged": reply.ok })))
        }
        "hang" => None,
        "exit" => std::process::exit(3),
        other => Some(PluginProcessMessage::Failure {
            id: id.to_owned(),
            name: "UnknownToolError".into(),
            message: format!("the fixture serves no tool named '{other}'"),
        }),
    }
}

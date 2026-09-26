//! The device side of a Swage device connection.
//!
//! A device is a process that runs tools on behalf of a gateway — on
//! another machine, in a container, over SSH. It never listens: it dials
//! the gateway's device route, presents its registration's shared token,
//! advertises the tools its dispatcher holds, and then serves the
//! operations the gateway sends over that connection, heartbeating while
//! it lives and dialling again when the connection drops. The wire contract
//! is `swage_protocol::device`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use futures::{FutureExt, SinkExt, StreamExt};
use serde_json::json;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::Message;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use swage_tool::context::{ToolContext, context_within_workspaces};
use swage_tool::dispatch::ToolDispatcher;

pub use swage_protocol::device::{
    DeviceFrame, DeviceResponse, GatewayFrame, RegisterDeviceRequest, SerializableToolContext,
};

/// The path a device dials on its gateway.
pub const CONNECT_PATH: &str = "/api/devices/connect";

/// How long the device waits between one connection ending and the next
/// dial when nothing better is configured.
pub const DEFAULT_RECONNECT_DELAY: Duration = Duration::from_secs(2);

#[derive(Debug, thiserror::Error)]
pub enum DeviceError {
    #[error("could not connect to {url}: {message}")]
    Connect { url: String, message: String },
    #[error("the gateway rejected this device: {0}")]
    Rejected(String),
    #[error("the gateway spoke outside the device contract: {0}")]
    Protocol(String),
    #[error("registration failed: {0}")]
    Registration(String),
}

/// One device's standing connection to its gateway.
pub struct DeviceConnection {
    /// The gateway's HTTP base URL (`http://host:port`); the device route is
    /// derived from it.
    pub gateway_url: String,
    pub device_id: String,
    /// The value of the gateway secret the registration named.
    pub shared_token: String,
    pub label: Option<String>,
    /// The tools this device runs; their names are what it advertises.
    pub dispatcher: Arc<ToolDispatcher>,
    /// The roots an operation's context must lie within; an operation that
    /// escapes them is refused here as well as at the gateway.
    pub workspace_roots: Vec<PathBuf>,
    pub reconnect_delay: Duration,
}

/// Why one connection ended, for the caller that decides whether to dial
/// again.
#[derive(Debug, PartialEq, Eq)]
pub enum ConnectionEnd {
    /// The gateway closed the connection or the transport dropped.
    Closed,
    /// The caller's shutdown token fired; the device said goodbye.
    Shutdown,
}

impl DeviceConnection {
    /// Register `request` with the gateway over HTTP. A device does this
    /// once (or redeems a pairing code instead); connecting needs only the
    /// shared token afterwards.
    pub async fn register(
        gateway_url: &str,
        request: &RegisterDeviceRequest,
    ) -> Result<DeviceResponse, DeviceError> {
        let url = format!("{}/api/devices/register", gateway_url.trim_end_matches('/'));
        let response = reqwest::Client::new()
            .post(&url)
            .json(request)
            .send()
            .await
            .map_err(|error| DeviceError::Registration(error.to_string()))?;
        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(DeviceError::Registration(format!(
                "{url} answered {status}: {body}"
            )));
        }
        response
            .json()
            .await
            .map_err(|error| DeviceError::Registration(error.to_string()))
    }

    /// Serve the gateway until `shutdown` fires: dial, serve one connection
    /// to its end, wait `reconnect_delay`, dial again. A rejected hello is
    /// returned rather than retried — a wrong credential does not fix
    /// itself.
    pub async fn run(self: Arc<Self>, shutdown: CancellationToken) -> Result<(), DeviceError> {
        loop {
            match self.connect_once(&shutdown).await {
                Ok(ConnectionEnd::Shutdown) => return Ok(()),
                Ok(ConnectionEnd::Closed) => {
                    tracing::info!(device_id = %self.device_id, "device connection closed; redialling");
                }
                Err(DeviceError::Rejected(message)) => return Err(DeviceError::Rejected(message)),
                Err(error) => {
                    tracing::warn!(device_id = %self.device_id, %error, "device connection failed; redialling");
                }
            }
            tokio::select! {
                _ = shutdown.cancelled() => return Ok(()),
                _ = tokio::time::sleep(self.reconnect_delay) => {}
            }
        }
    }

    /// Dial the gateway and serve one connection to its end.
    pub async fn connect_once(
        &self,
        shutdown: &CancellationToken,
    ) -> Result<ConnectionEnd, DeviceError> {
        let url = connect_url(&self.gateway_url);
        let (socket, _) = tokio_tungstenite::connect_async(&url)
            .await
            .map_err(|error| DeviceError::Connect {
                url: url.clone(),
                message: error.to_string(),
            })?;
        let (mut sink, mut stream) = socket.split();

        send_frame(
            &mut sink,
            &DeviceFrame::Hello {
                device_id: self.device_id.clone(),
                shared_token: self.shared_token.clone(),
                label: self.label.clone(),
                capabilities: self.capabilities(),
            },
        )
        .await?;

        let heartbeat_interval_secs = match next_gateway_frame(&mut stream).await? {
            Some(GatewayFrame::Welcome {
                heartbeat_interval_secs,
                ..
            }) => heartbeat_interval_secs,
            Some(GatewayFrame::Rejected { message }) => return Err(DeviceError::Rejected(message)),
            Some(other) => {
                return Err(DeviceError::Protocol(format!(
                    "expected a welcome, received {}",
                    frame_kind(&other)
                )));
            }
            None => return Ok(ConnectionEnd::Closed),
        };
        tracing::info!(device_id = %self.device_id, %url, "device connected");

        let (outbound_tx, mut outbound_rx) = mpsc::unbounded_channel::<DeviceFrame>();
        let mut heartbeat =
            tokio::time::interval(Duration::from_secs(heartbeat_interval_secs.max(1)));
        heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        heartbeat.tick().await;
        let mut running: HashMap<Uuid, JoinHandle<()>> = HashMap::new();

        let end = loop {
            tokio::select! {
                _ = shutdown.cancelled() => break ConnectionEnd::Shutdown,
                _ = heartbeat.tick() => {
                    if send_frame(&mut sink, &DeviceFrame::Heartbeat {}).await.is_err() {
                        break ConnectionEnd::Closed;
                    }
                }
                Some(frame) = outbound_rx.recv() => {
                    if let Some(correlation_id) = settled_correlation_id(&frame) {
                        running.remove(&correlation_id);
                    }
                    if send_frame(&mut sink, &frame).await.is_err() {
                        break ConnectionEnd::Closed;
                    }
                }
                message = stream.next() => {
                    let frame = match message {
                        Some(Ok(Message::Text(text))) => match serde_json::from_str::<GatewayFrame>(&text) {
                            Ok(frame) => frame,
                            Err(error) => {
                                tracing::warn!(device_id = %self.device_id, %error, "ignoring a frame outside the device contract");
                                continue;
                            }
                        },
                        Some(Ok(Message::Close(_))) | None => break ConnectionEnd::Closed,
                        Some(Err(error)) => {
                            tracing::warn!(device_id = %self.device_id, %error, "device connection errored");
                            break ConnectionEnd::Closed;
                        }
                        Some(Ok(_)) => continue,
                    };
                    match frame {
                        GatewayFrame::Operation { correlation_id, tool_name, params, context } => {
                            let handle = self.start_operation(correlation_id, tool_name, params, context, outbound_tx.clone());
                            running.insert(correlation_id, handle);
                        }
                        GatewayFrame::Cancel { correlation_id } => {
                            // An operation that already finished has sent its
                            // result and left `running`; only a live one is
                            // stopped and answered as cancelled.
                            if let Some(handle) = running.remove(&correlation_id) {
                                handle.abort();
                                let _ = outbound_tx.send(DeviceFrame::Cancelled { correlation_id });
                            }
                        }
                        GatewayFrame::Welcome { .. } | GatewayFrame::Rejected { .. } => {
                            tracing::warn!(device_id = %self.device_id, "ignoring a handshake frame after the handshake");
                        }
                    }
                }
            }
        };

        for (_, handle) in running.drain() {
            handle.abort();
        }
        let _ = sink.send(Message::Close(None)).await;
        Ok(end)
    }

    /// The tool names this device advertises: everything its dispatcher
    /// holds.
    pub fn capabilities(&self) -> Vec<String> {
        let mut names = self
            .dispatcher
            .registry()
            .list()
            .into_iter()
            .map(ToOwned::to_owned)
            .collect::<Vec<_>>();
        names.sort();
        names
    }

    fn start_operation(
        &self,
        correlation_id: Uuid,
        tool_name: String,
        params: serde_json::Value,
        context: Box<SerializableToolContext>,
        outbound: mpsc::UnboundedSender<DeviceFrame>,
    ) -> JoinHandle<()> {
        let dispatcher = Arc::clone(&self.dispatcher);
        let workspace_roots = self.workspace_roots.clone();
        let device_id = self.device_id.clone();
        tokio::spawn(async move {
            let frame = if !context_within_workspaces(&*context, &workspace_roots) {
                DeviceFrame::Failure {
                    correlation_id,
                    message: format!(
                        "the operation's context escapes device '{device_id}' workspace"
                    ),
                }
            } else {
                // A tool that panics still answers: an operation with no
                // settling frame would leave the gateway's caller waiting
                // until the link itself closed.
                let dispatched = std::panic::AssertUnwindSafe(dispatcher.dispatch(
                    &tool_name,
                    params,
                    &*context,
                    context.agent_id(),
                ))
                .catch_unwind()
                .await;
                match dispatched {
                    Ok(Ok(output)) => DeviceFrame::Result {
                        correlation_id,
                        output: output.result,
                    },
                    Ok(Err(error)) => DeviceFrame::Failure {
                        correlation_id,
                        message: error.to_string(),
                    },
                    Err(_) => DeviceFrame::Failure {
                        correlation_id,
                        message: format!("tool '{tool_name}' panicked on device '{device_id}'"),
                    },
                }
            };
            let _ = outbound.send(frame);
        })
    }
}

/// The device route's URL for a gateway base URL: `http` becomes `ws`.
pub fn connect_url(gateway_url: &str) -> String {
    let base = gateway_url.trim_end_matches('/');
    let socket_base = if let Some(rest) = base.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if let Some(rest) = base.strip_prefix("http://") {
        format!("ws://{rest}")
    } else {
        base.to_owned()
    };
    format!("{socket_base}{CONNECT_PATH}")
}

fn settled_correlation_id(frame: &DeviceFrame) -> Option<Uuid> {
    match frame {
        DeviceFrame::Result { correlation_id, .. }
        | DeviceFrame::Failure { correlation_id, .. }
        | DeviceFrame::Cancelled { correlation_id } => Some(*correlation_id),
        DeviceFrame::Hello { .. } | DeviceFrame::Heartbeat {} => None,
    }
}

fn frame_kind(frame: &GatewayFrame) -> &'static str {
    match frame {
        GatewayFrame::Welcome { .. } => "welcome",
        GatewayFrame::Rejected { .. } => "rejected",
        GatewayFrame::Operation { .. } => "operation",
        GatewayFrame::Cancel { .. } => "cancel",
    }
}

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn send_frame(
    sink: &mut futures::stream::SplitSink<Socket, Message>,
    frame: &DeviceFrame,
) -> Result<(), DeviceError> {
    let text = serde_json::to_string(frame).unwrap_or_else(|_| json!({}).to_string());
    sink.send(Message::Text(text.into()))
        .await
        .map_err(|error| DeviceError::Protocol(format!("send failed: {error}")))
}

async fn next_gateway_frame(
    stream: &mut futures::stream::SplitStream<Socket>,
) -> Result<Option<GatewayFrame>, DeviceError> {
    loop {
        match stream.next().await {
            Some(Ok(Message::Text(text))) => {
                return serde_json::from_str(&text)
                    .map(Some)
                    .map_err(|error| DeviceError::Protocol(error.to_string()));
            }
            Some(Ok(Message::Close(_))) | None => return Ok(None),
            Some(Ok(_)) => continue,
            Some(Err(error)) => return Err(DeviceError::Protocol(error.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use serde_json::Value;
    use swage_core::ToolError;
    use swage_tool::tool::Tool;
    use tokio::net::TcpListener;

    struct EchoTool;

    #[async_trait]
    impl Tool for EchoTool {
        fn name(&self) -> &str {
            "echo"
        }
        fn description(&self) -> &str {
            "Returns its parameters."
        }
        fn parameters(&self) -> Value {
            json!({ "type": "object", "properties": { "text": { "type": "string" } } })
        }
        async fn execute(&self, params: Value, _ctx: &dyn ToolContext) -> Result<Value, ToolError> {
            Ok(json!({ "echoed": params["text"] }))
        }
    }

    struct PanickingTool;

    #[async_trait]
    impl Tool for PanickingTool {
        fn name(&self) -> &str {
            "explode"
        }
        fn description(&self) -> &str {
            "Panics."
        }
        fn parameters(&self) -> Value {
            json!({ "type": "object", "properties": {} })
        }
        async fn execute(
            &self,
            _params: Value,
            _ctx: &dyn ToolContext,
        ) -> Result<Value, ToolError> {
            panic!("boom");
        }
    }

    struct SlowTool;

    #[async_trait]
    impl Tool for SlowTool {
        fn name(&self) -> &str {
            "slow"
        }
        fn description(&self) -> &str {
            "Takes a long time."
        }
        fn parameters(&self) -> Value {
            json!({ "type": "object", "properties": {} })
        }
        async fn execute(
            &self,
            _params: Value,
            _ctx: &dyn ToolContext,
        ) -> Result<Value, ToolError> {
            tokio::time::sleep(Duration::from_secs(30)).await;
            Ok(json!({ "finished": true }))
        }
    }

    fn connection(gateway_url: String, workspace: &std::path::Path) -> Arc<DeviceConnection> {
        Arc::new(DeviceConnection {
            gateway_url,
            device_id: "device-a".into(),
            shared_token: "token-a".into(),
            label: Some("Device A".into()),
            dispatcher: Arc::new(ToolDispatcher::allow_all(vec![
                Arc::new(EchoTool) as Arc<dyn Tool>,
                Arc::new(SlowTool) as Arc<dyn Tool>,
                Arc::new(PanickingTool) as Arc<dyn Tool>,
            ])),
            workspace_roots: vec![workspace.to_path_buf()],
            reconnect_delay: Duration::from_millis(10),
        })
    }

    fn context_in(workspace: &std::path::Path) -> SerializableToolContext {
        SerializableToolContext {
            agent_id: "gateway".into(),
            session_id: "s".into(),
            working_directory: workspace.to_path_buf(),
            allowed_paths: vec![],
            user_info: None,
            call_id: None,
            turn_id: None,
        }
    }

    type ServerSocket = tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>;

    /// A gateway stand-in: accept one device connection and return the
    /// socket plus the device's hello.
    async fn accept_device(listener: &TcpListener) -> (ServerSocket, DeviceFrame) {
        let (tcp, _) = listener.accept().await.unwrap();
        let mut socket = tokio_tungstenite::accept_async(tcp).await.unwrap();
        let hello = match socket.next().await {
            Some(Ok(Message::Text(text))) => serde_json::from_str::<DeviceFrame>(&text).unwrap(),
            other => panic!("expected a hello frame, got {other:?}"),
        };
        (socket, hello)
    }

    async fn gateway_send(socket: &mut ServerSocket, frame: &GatewayFrame) {
        socket
            .send(Message::Text(serde_json::to_string(frame).unwrap().into()))
            .await
            .unwrap();
    }

    async fn gateway_recv(socket: &mut ServerSocket) -> DeviceFrame {
        loop {
            match socket.next().await {
                Some(Ok(Message::Text(text))) => return serde_json::from_str(&text).unwrap(),
                Some(Ok(_)) => continue,
                other => panic!("device connection ended: {other:?}"),
            }
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_device_advertises_on_hello_then_answers_operations_and_cancellations() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let workspace = std::env::temp_dir();
        let connection = connection(format!("http://{address}"), &workspace);
        let shutdown = CancellationToken::new();
        let device_task = {
            let connection = Arc::clone(&connection);
            let shutdown = shutdown.clone();
            tokio::spawn(async move { connection.connect_once(&shutdown).await })
        };

        let (mut socket, hello) = accept_device(&listener).await;
        assert_eq!(
            hello,
            DeviceFrame::Hello {
                device_id: "device-a".into(),
                shared_token: "token-a".into(),
                label: Some("Device A".into()),
                capabilities: vec!["echo".into(), "explode".into(), "slow".into()],
            }
        );
        gateway_send(
            &mut socket,
            &GatewayFrame::Welcome {
                device_id: "device-a".into(),
                heartbeat_interval_secs: 1,
            },
        )
        .await;

        // An operation is answered with its result under the same id.
        let echo_id = Uuid::new_v4();
        gateway_send(
            &mut socket,
            &GatewayFrame::Operation {
                correlation_id: echo_id,
                tool_name: "echo".into(),
                params: json!({ "text": "hello" }),
                context: Box::new(context_in(&workspace)),
            },
        )
        .await;
        let answer = loop {
            let frame = gateway_recv(&mut socket).await;
            if frame != (DeviceFrame::Heartbeat {}) {
                break frame;
            }
        };
        assert_eq!(
            answer,
            DeviceFrame::Result {
                correlation_id: echo_id,
                output: json!({ "echoed": "hello" }),
            }
        );

        // A cancelled operation is stopped and answered as cancelled.
        let slow_id = Uuid::new_v4();
        gateway_send(
            &mut socket,
            &GatewayFrame::Operation {
                correlation_id: slow_id,
                tool_name: "slow".into(),
                params: json!({}),
                context: Box::new(context_in(&workspace)),
            },
        )
        .await;
        gateway_send(
            &mut socket,
            &GatewayFrame::Cancel {
                correlation_id: slow_id,
            },
        )
        .await;
        let answer = loop {
            let frame = gateway_recv(&mut socket).await;
            if frame != (DeviceFrame::Heartbeat {}) {
                break frame;
            }
        };
        assert_eq!(
            answer,
            DeviceFrame::Cancelled {
                correlation_id: slow_id
            }
        );

        // A context outside the device's workspace is refused on the device.
        let outside_id = Uuid::new_v4();
        let mut outside = context_in(&workspace);
        outside.working_directory = PathBuf::from("/");
        gateway_send(
            &mut socket,
            &GatewayFrame::Operation {
                correlation_id: outside_id,
                tool_name: "echo".into(),
                params: json!({}),
                context: Box::new(outside),
            },
        )
        .await;
        let answer = loop {
            let frame = gateway_recv(&mut socket).await;
            if frame != (DeviceFrame::Heartbeat {}) {
                break frame;
            }
        };
        assert!(matches!(
            answer,
            DeviceFrame::Failure { correlation_id, ref message }
                if correlation_id == outside_id && message.contains("escapes")
        ));

        // A tool that panics still settles its operation, as a failure.
        let explode_id = Uuid::new_v4();
        gateway_send(
            &mut socket,
            &GatewayFrame::Operation {
                correlation_id: explode_id,
                tool_name: "explode".into(),
                params: json!({}),
                context: Box::new(context_in(&workspace)),
            },
        )
        .await;
        let answer = loop {
            let frame = gateway_recv(&mut socket).await;
            if frame != (DeviceFrame::Heartbeat {}) {
                break frame;
            }
        };
        assert!(matches!(
            answer,
            DeviceFrame::Failure { correlation_id, ref message }
                if correlation_id == explode_id && message.contains("panicked")
        ));

        // Heartbeats keep arriving while the connection lives.
        let heartbeat = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if gateway_recv(&mut socket).await == (DeviceFrame::Heartbeat {}) {
                    break;
                }
            }
        })
        .await;
        assert!(heartbeat.is_ok(), "no heartbeat within three seconds");

        shutdown.cancel();
        assert_eq!(device_task.await.unwrap().unwrap(), ConnectionEnd::Shutdown);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_rejected_hello_is_an_error_and_a_closed_connection_is_reported_closed() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let workspace = std::env::temp_dir();
        let connection = connection(format!("http://{address}"), &workspace);
        let shutdown = CancellationToken::new();

        let rejected = {
            let connection = Arc::clone(&connection);
            let shutdown = shutdown.clone();
            tokio::spawn(async move { connection.connect_once(&shutdown).await })
        };
        let (mut socket, _) = accept_device(&listener).await;
        gateway_send(
            &mut socket,
            &GatewayFrame::Rejected {
                message: "invalid shared token".into(),
            },
        )
        .await;
        let _ = socket.close(None).await;
        assert!(matches!(
            rejected.await.unwrap(),
            Err(DeviceError::Rejected(message)) if message == "invalid shared token"
        ));

        let closed = {
            let connection = Arc::clone(&connection);
            let shutdown = shutdown.clone();
            tokio::spawn(async move { connection.connect_once(&shutdown).await })
        };
        let (mut socket, _) = accept_device(&listener).await;
        gateway_send(
            &mut socket,
            &GatewayFrame::Welcome {
                device_id: "device-a".into(),
                heartbeat_interval_secs: 30,
            },
        )
        .await;
        socket.close(None).await.unwrap();
        assert_eq!(closed.await.unwrap().unwrap(), ConnectionEnd::Closed);
    }

    #[test]
    fn the_connect_url_swaps_the_scheme_and_appends_the_device_route() {
        assert_eq!(
            connect_url("http://127.0.0.1:3000/"),
            "ws://127.0.0.1:3000/api/devices/connect"
        );
        assert_eq!(
            connect_url("https://gateway.example"),
            "wss://gateway.example/api/devices/connect"
        );
    }
}

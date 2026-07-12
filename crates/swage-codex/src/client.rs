use std::{collections::HashMap, sync::Arc, time::Duration};

use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{ChildStdin, ChildStdout},
    sync::{Mutex, broadcast, oneshot},
};

use crate::{JsonRpcError, RequestId};

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("app-server transport closed")]
    Closed,
    #[error("app-server request timed out: {method}")]
    Timeout { method: String },
    #[error("app-server I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("app-server returned {code}: {message}")]
    Rpc { code: i64, message: String },
    #[error("app-server returned malformed JSON: {0}")]
    Malformed(String),
}

#[derive(Debug, Clone)]
pub struct ServerEvent {
    pub method: String,
    pub params: Value,
}

#[derive(Debug, Clone)]
pub struct ServerRequest {
    pub id: RequestId,
    pub method: String,
    pub params: Value,
}

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value, ClientError>>>>>;

/// Multiplexed line-delimited JSON-RPC client.
#[derive(Clone)]
pub struct AppServerClient {
    writer: Arc<Mutex<ChildStdin>>,
    pending: Pending,
    next_id: Arc<std::sync::atomic::AtomicU64>,
    notifications: broadcast::Sender<ServerEvent>,
    server_requests: broadcast::Sender<ServerRequest>,
    timeout: Duration,
}

impl AppServerClient {
    pub fn new(stdin: ChildStdin, stdout: ChildStdout, timeout: Duration) -> Self {
        let (notifications, _) = broadcast::channel(512);
        let (server_requests, _) = broadcast::channel(128);
        let client = Self {
            writer: Arc::new(Mutex::new(stdin)),
            pending: Arc::new(Mutex::new(HashMap::new())),
            next_id: Arc::new(std::sync::atomic::AtomicU64::new(1)),
            notifications,
            server_requests,
            timeout,
        };
        client.spawn_reader(stdout);
        client
    }

    pub fn subscribe(&self) -> broadcast::Receiver<ServerEvent> {
        self.notifications.subscribe()
    }

    pub fn subscribe_requests(&self) -> broadcast::Receiver<ServerRequest> {
        self.server_requests.subscribe()
    }

    pub async fn request(&self, method: &str, params: Value) -> Result<Value, ClientError> {
        let id = self
            .next_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);
        self.write(&json!({"jsonrpc":"2.0", "id":id, "method":method, "params":params}))
            .await?;
        match tokio::time::timeout(self.timeout, rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(ClientError::Closed),
            Err(_) => {
                self.pending.lock().await.remove(&id);
                Err(ClientError::Timeout {
                    method: method.to_string(),
                })
            }
        }
    }

    pub async fn notify(&self, method: &str, params: Value) -> Result<(), ClientError> {
        self.write(&json!({"jsonrpc":"2.0", "method":method, "params":params}))
            .await
    }

    pub async fn respond(&self, id: RequestId, result: Value) -> Result<(), ClientError> {
        self.write(&json!({"jsonrpc":"2.0", "id":id, "result":result}))
            .await
    }

    pub async fn respond_error(
        &self,
        id: RequestId,
        code: i64,
        message: &str,
    ) -> Result<(), ClientError> {
        self.write(&json!({"jsonrpc":"2.0", "id":id, "error":{"code":code,"message":message}}))
            .await
    }

    async fn write(&self, value: &Value) -> Result<(), ClientError> {
        let mut frame =
            serde_json::to_vec(value).map_err(|error| ClientError::Malformed(error.to_string()))?;
        frame.push(b'\n');
        let mut writer = self.writer.lock().await;
        writer.write_all(&frame).await?;
        writer.flush().await?;
        Ok(())
    }

    fn spawn_reader(&self, stdout: ChildStdout) {
        let pending = self.pending.clone();
        let notifications = self.notifications.clone();
        let server_requests = self.server_requests.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let Ok(frame) = serde_json::from_str::<Value>(&line) else {
                    tracing::warn!(frame = %line, "ignoring malformed app-server frame");
                    continue;
                };
                if let Some(id) = frame.get("id").and_then(Value::as_u64)
                    && (frame.get("result").is_some() || frame.get("error").is_some())
                {
                    if let Some(tx) = pending.lock().await.remove(&id) {
                        let result = match frame.get("error") {
                            Some(error) => serde_json::from_value::<JsonRpcError>(error.clone())
                                .map(|error| {
                                    Err(ClientError::Rpc {
                                        code: error.code,
                                        message: error.message,
                                    })
                                })
                                .unwrap_or_else(|error| {
                                    Err(ClientError::Malformed(error.to_string()))
                                }),
                            None => Ok(frame.get("result").cloned().unwrap_or(Value::Null)),
                        };
                        let _ = tx.send(result);
                    }
                    continue;
                }
                let Some(method) = frame.get("method").and_then(Value::as_str) else {
                    continue;
                };
                let params = frame.get("params").cloned().unwrap_or(Value::Null);
                if let Some(id) = frame.get("id") {
                    if let Ok(id) = serde_json::from_value::<RequestId>(id.clone()) {
                        let _ = server_requests.send(ServerRequest {
                            id,
                            method: method.to_string(),
                            params,
                        });
                    }
                } else {
                    let _ = notifications.send(ServerEvent {
                        method: method.to_string(),
                        params,
                    });
                }
            }
            let mut pending = pending.lock().await;
            for (_, tx) in pending.drain() {
                let _ = tx.send(Err(ClientError::Closed));
            }
        });
    }
}

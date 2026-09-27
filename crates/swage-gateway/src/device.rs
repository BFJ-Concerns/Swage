//! Remote execution devices: registration, the connection each device
//! dials in on, and the operations the gateway invokes over it.
//!
//! A device pairs (or registers) once, naming the gateway secret its
//! credential resolves against and the workspace it operates in. It then
//! dials `GET /api/devices/connect`, opens with a `Hello` carrying that
//! credential and the tools it can run, and the gateway serves operations
//! back over the same socket: each `Operation` carries a correlation id, is
//! settled by the device's `Result`, `Failure`, or `Cancelled` frame, and is
//! cancelled by dropping the awaiting future, which sends `Cancel`. The
//! device heartbeats over the socket; presence reads those. Every connect,
//! disconnect, invocation, and settlement lands in the audit trail under the
//! device's id, so an operation's outcome is reconstructable afterwards
//! from the gateway's own record.

use std::collections::HashMap;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::{extract::State, response::Response};
use chrono::{DateTime, Utc};
use futures::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;

use swage_core::ToolError;
use swage_storage::model::{NewAuditEvent, NewDeviceRegistration};
use swage_tool::context::{ToolContext, context_within_workspaces, serialise_tool_context};

use crate::state::AppState;

pub use swage_protocol::device::{
    DeviceFrame, DeviceListResponse, DeviceResponse, GatewayFrame, RegisterDeviceRequest,
};

/// How often a connected device heartbeats, as the welcome tells it.
pub const HEARTBEAT_INTERVAL_SECS: u64 = 10;
/// How long presence tolerates silence before reading the device as stale.
pub const STALE_AFTER_SECS: u64 = 3 * HEARTBEAT_INTERVAL_SECS;
/// How long a fresh connection has to say hello.
const HELLO_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DeviceKind {
    Local,
    AttachedDevice,
    Docker,
    Ssh,
}

impl std::fmt::Display for DeviceKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let value = match self {
            DeviceKind::Local => "local",
            DeviceKind::AttachedDevice => "attached_device",
            DeviceKind::Docker => "docker",
            DeviceKind::Ssh => "ssh",
        };
        write!(f, "{value}")
    }
}

impl std::str::FromStr for DeviceKind {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "local" => Ok(Self::Local),
            "attached_device" => Ok(Self::AttachedDevice),
            "docker" => Ok(Self::Docker),
            "ssh" => Ok(Self::Ssh),
            other => Err(format!("unknown device kind: {other}")),
        }
    }
}

#[derive(Debug, Clone)]
pub struct RegisteredDevice {
    pub device_id: String,
    pub label: Option<String>,
    pub kind: DeviceKind,
    pub shared_token_secret_ref: Option<String>,
    pub workspace_root: PathBuf,
    pub workspace_roots: Vec<PathBuf>,
    /// What the device advertised when it last connected.
    pub capabilities: Vec<String>,
    pub registered_at: DateTime<Utc>,
}

/// How an invoked operation ended, as the device reported it or as the
/// link's end forced it.
#[derive(Debug, Clone, PartialEq)]
pub enum OperationOutcome {
    Result(Value),
    Failure(String),
    Cancelled,
    Disconnected,
}

impl OperationOutcome {
    fn audit_name(&self) -> &'static str {
        match self {
            OperationOutcome::Result(_) => "result",
            OperationOutcome::Failure(_) => "failure",
            OperationOutcome::Cancelled => "cancelled",
            OperationOutcome::Disconnected => "disconnected",
        }
    }
}

/// One device's live connection: the frames the gateway may send it and
/// the operations awaiting its answer.
#[derive(Clone)]
pub struct DeviceLink {
    link_id: Uuid,
    outbound: mpsc::UnboundedSender<GatewayFrame>,
    pending: Arc<Mutex<HashMap<Uuid, oneshot::Sender<OperationOutcome>>>>,
    /// Fired to close the connection from the gateway's side — an
    /// operator's disconnect, or a newer dial from the same device.
    closed: tokio_util::sync::CancellationToken,
}

impl DeviceLink {
    fn settle(&self, correlation_id: Uuid, outcome: OperationOutcome) -> bool {
        let waiter = self
            .pending
            .lock()
            .expect("pending operations lock")
            .remove(&correlation_id);
        match waiter {
            Some(waiter) => waiter.send(outcome).is_ok(),
            None => false,
        }
    }

    /// Settle every pending operation with `outcome`, returning their ids
    /// so the caller can record each settlement.
    fn fail_all(&self, outcome: OperationOutcome) -> Vec<Uuid> {
        let waiters = std::mem::take(&mut *self.pending.lock().expect("pending operations lock"));
        let mut settled = Vec::with_capacity(waiters.len());
        for (correlation_id, waiter) in waiters {
            let _ = waiter.send(outcome.clone());
            settled.push(correlation_id);
        }
        settled
    }
}

#[derive(Debug, Default)]
pub struct DeviceRuntime {
    pub devices: HashMap<String, RegisteredDevice>,
    /// The connection each device currently holds open, by device id.
    pub links: HashMap<String, DeviceLink>,
}

impl std::fmt::Debug for DeviceLink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeviceLink")
            .field("link_id", &self.link_id)
            .finish_non_exhaustive()
    }
}

// ---------------------------------------------------------------------------
// Registration
// ---------------------------------------------------------------------------

pub async fn register_remote_device(
    state: &AppState,
    request: RegisterDeviceRequest,
) -> Result<RegisteredDevice, swage_core::GatewayError> {
    let kind = remote_registration_kind(&request.kind)?;
    let workspace_root = PathBuf::from(&request.workspace_root);
    let workspace_roots =
        normalise_workspace_roots(workspace_root.clone(), request.workspace_roots);
    // A re-registration keeps what the device last advertised; a new
    // device has advertised nothing yet.
    let previously_advertised = state
        .devices
        .lock()
        .await
        .devices
        .get(&request.device_id)
        .map(|device| device.capabilities.clone())
        .unwrap_or_default();
    let device = RegisteredDevice {
        device_id: request.device_id,
        label: request.label.clone(),
        kind,
        shared_token_secret_ref: Some(request.shared_token_secret_ref),
        workspace_root,
        workspace_roots,
        capabilities: previously_advertised,
        registered_at: Utc::now(),
    };

    state
        .storage
        .upsert_device(NewDeviceRegistration {
            device_id: device.device_id.clone(),
            label: device.label.clone(),
            kind: device.kind.to_string(),
            shared_token: device.shared_token_secret_ref.clone().unwrap_or_default(),
            workspace_root: device.workspace_root.to_string_lossy().into_owned(),
            workspace_roots: device
                .workspace_roots
                .iter()
                .map(|path| path.to_string_lossy().into_owned())
                .collect(),
            capabilities: device.capabilities.clone(),
        })
        .await
        .map_err(swage_core::GatewayError::Storage)?;

    state
        .devices
        .lock()
        .await
        .devices
        .insert(device.device_id.clone(), device.clone());

    Ok(device)
}

/// Parse the kind at the shared remote-registration boundary.
///
/// Local execution has no remote producer, so pairing and recovery imports
/// must receive the same refusal as HTTP registration.
fn remote_registration_kind(value: &str) -> Result<DeviceKind, swage_core::GatewayError> {
    let kind = DeviceKind::from_str(value.trim()).map_err(|_| {
        swage_core::GatewayError::ValidationError(format!("unknown device kind '{value}'"))
    })?;
    if kind == DeviceKind::Local {
        return Err(swage_core::GatewayError::ValidationError(
            "a local device has no remote producer to register".into(),
        ));
    }
    Ok(kind)
}

pub async fn restore_registered_devices(state: &AppState) -> Result<(), swage_core::StorageError> {
    let persisted_devices = state.storage.list_devices().await?;
    let mut runtime = state.devices.lock().await;

    for device in persisted_devices {
        runtime.devices.insert(
            device.device_id.clone(),
            RegisteredDevice {
                device_id: device.device_id,
                label: device.label,
                kind: DeviceKind::from_str(&device.kind).unwrap_or(DeviceKind::AttachedDevice),
                shared_token_secret_ref: Some(device.shared_token),
                workspace_root: PathBuf::from(&device.workspace_root),
                workspace_roots: normalise_workspace_roots(
                    PathBuf::from(&device.workspace_root),
                    device.workspace_roots,
                ),
                capabilities: device.capabilities,
                registered_at: device.registered_at,
            },
        );
    }

    Ok(())
}

pub fn device_response(device: RegisteredDevice) -> DeviceResponse {
    DeviceResponse {
        device_id: device.device_id,
        label: device.label,
        kind: device.kind.to_string(),
        workspace_root: device.workspace_root.to_string_lossy().into_owned(),
        workspace_roots: device
            .workspace_roots
            .iter()
            .map(|path| path.to_string_lossy().into_owned())
            .collect(),
        capabilities: device.capabilities,
        registered_at: device.registered_at,
    }
}

pub async fn list_devices(state: &AppState) -> DeviceListResponse {
    let runtime = state.devices.lock().await;
    let mut devices = runtime
        .devices
        .values()
        .cloned()
        .map(device_response)
        .collect::<Vec<_>>();
    devices.sort_by(|left, right| left.device_id.cmp(&right.device_id));
    DeviceListResponse { devices }
}

pub async fn resolve_device(
    state: &AppState,
    session_metadata: Option<&serde_json::Value>,
) -> Option<RegisteredDevice> {
    let runtime = state.devices.lock().await;
    select_device(&runtime.devices, session_metadata)
}

fn select_device(
    devices: &HashMap<String, RegisteredDevice>,
    session_metadata: Option<&serde_json::Value>,
) -> Option<RegisteredDevice> {
    if let Some(device_id) = session_metadata
        .and_then(|metadata| metadata.get("device_id"))
        .and_then(|value| value.as_str())
        && let Some(device) = devices.get(device_id)
    {
        return Some(device.clone());
    }

    if let Some(preferred_kind) = session_metadata
        .and_then(|metadata| metadata.get("execution_backend"))
        .and_then(|value| value.as_str())
        .and_then(|value| DeviceKind::from_str(value).ok())
    {
        let mut matching_devices = devices
            .values()
            .filter(|device| device.kind == preferred_kind)
            .cloned()
            .collect::<Vec<_>>();
        matching_devices.sort_by(|left, right| left.device_id.cmp(&right.device_id));
        if let Some(device) = matching_devices.into_iter().next() {
            return Some(device);
        }
    }

    None
}

fn normalise_workspace_roots(primary: PathBuf, roots: Vec<String>) -> Vec<PathBuf> {
    let mut normalised = vec![primary];
    for root in roots {
        let path = PathBuf::from(root);
        if !normalised.contains(&path) {
            normalised.push(path);
        }
    }
    normalised
}

// ---------------------------------------------------------------------------
// Invoking an operation over the device's connection
// ---------------------------------------------------------------------------

/// Run `tool_name` on `device` over the connection it holds open.
///
/// Refuses, before touching the link, a tool the device did not advertise
/// and a context outside its workspace; refuses a device that is not
/// connected. Dropping the returned future before it settles cancels the
/// operation on the device.
pub async fn execute_on_remote_device(
    state: &AppState,
    device: &RegisteredDevice,
    tool_name: &str,
    params: &Value,
    ctx: &dyn ToolContext,
) -> Result<Value, ToolError> {
    let failed = |message: String| ToolError::ExecutionFailed {
        name: tool_name.to_string(),
        message,
    };
    if device.kind == DeviceKind::Local {
        return Err(failed(format!(
            "device '{}' uses the local backend and cannot be called through transport",
            device.device_id
        )));
    }
    if !device
        .capabilities
        .iter()
        .any(|capability| capability == tool_name)
    {
        return Err(failed(format!(
            "device '{}' does not expose tool '{}'",
            device.device_id, tool_name
        )));
    }
    if !context_within_workspaces(ctx, &device.workspace_roots) {
        return Err(failed(format!(
            "gateway context for '{}' escapes device workspace",
            device.device_id
        )));
    }

    let link = state
        .devices
        .lock()
        .await
        .links
        .get(&device.device_id)
        .cloned()
        .ok_or_else(|| failed(format!("device '{}' is not connected", device.device_id)))?;

    let correlation_id = Uuid::new_v4();
    let (settle_tx, settle_rx) = oneshot::channel();
    link.pending
        .lock()
        .expect("pending operations lock")
        .insert(correlation_id, settle_tx);
    // Cancellation is the drop of this future, from this point on: the
    // guard is installed before any await so no window exists in which the
    // operation is pending but nothing would send `Cancel` and record it.
    let mut in_flight = InFlightOperation {
        link: link.clone(),
        correlation_id,
        device_id: device.device_id.clone(),
        storage: Arc::clone(&state.storage),
        settled: false,
    };
    record_device_event(
        state,
        &device.device_id,
        "device_operation_invoked",
        format!(
            "Invoked '{tool_name}' on device '{}' (operation {correlation_id})",
            device.device_id
        ),
        serde_json::json!({
            "correlation_id": correlation_id,
            "tool_name": tool_name,
            "session_id": ctx.session_id(),
            "call_id": ctx.call_id(),
        }),
    )
    .await;

    let sent = link.outbound.send(GatewayFrame::Operation {
        correlation_id,
        tool_name: tool_name.to_string(),
        params: params.clone(),
        context: Box::new(serialise_tool_context(ctx)),
    });
    if sent.is_err() {
        in_flight.settled = true;
        link.settle(correlation_id, OperationOutcome::Disconnected);
        record_settlement(
            &state.storage,
            &device.device_id,
            correlation_id,
            &OperationOutcome::Disconnected,
        )
        .await;
        return Err(failed(format!(
            "device '{}' disconnected before the operation was sent",
            device.device_id
        )));
    }

    // The settlement is recorded where the device's answer is received
    // (the link loop), not here: a future dropped after the device answered
    // but before this await resumed would otherwise leave the answer
    // unrecorded.
    let outcome = settle_rx.await.unwrap_or(OperationOutcome::Disconnected);
    in_flight.settled = true;

    match outcome {
        OperationOutcome::Result(output) => Ok(output),
        OperationOutcome::Failure(message) => Err(failed(format!(
            "device '{}' failed the operation: {message}",
            device.device_id
        ))),
        OperationOutcome::Cancelled => Err(failed(format!(
            "operation on device '{}' was cancelled",
            device.device_id
        ))),
        OperationOutcome::Disconnected => Err(failed(format!(
            "device '{}' disconnected before the operation settled",
            device.device_id
        ))),
    }
}

/// An operation awaiting its device. Dropped unsettled — the awaiting turn
/// was cancelled — it tells the device to stop and records the cancellation.
struct InFlightOperation {
    link: DeviceLink,
    correlation_id: Uuid,
    device_id: String,
    storage: Arc<dyn swage_storage::store::Storage>,
    settled: bool,
}

impl Drop for InFlightOperation {
    fn drop(&mut self) {
        if self.settled {
            return;
        }
        let was_pending = self
            .link
            .pending
            .lock()
            .expect("pending operations lock")
            .remove(&self.correlation_id)
            .is_some();
        if !was_pending {
            return;
        }
        let _ = self.link.outbound.send(GatewayFrame::Cancel {
            correlation_id: self.correlation_id,
        });
        let storage = Arc::clone(&self.storage);
        let event = settlement_event(
            &self.device_id,
            self.correlation_id,
            &OperationOutcome::Cancelled,
        );
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                if let Err(error) = storage.create_audit_event(event).await {
                    tracing::warn!(%error, "failed to record a device operation's cancellation");
                }
            });
        }
    }
}

/// The one record of how an operation ended, written by whoever settles
/// it: the link loop for a device's answer, the guard for a cancellation,
/// the link's end for a disconnection.
async fn record_settlement(
    storage: &Arc<dyn swage_storage::store::Storage>,
    device_id: &str,
    correlation_id: Uuid,
    outcome: &OperationOutcome,
) {
    if let Err(error) = storage
        .create_audit_event(settlement_event(device_id, correlation_id, outcome))
        .await
    {
        tracing::warn!(%device_id, %correlation_id, %error, "failed to record a device operation's settlement");
    }
}

fn settlement_event(
    device_id: &str,
    correlation_id: Uuid,
    outcome: &OperationOutcome,
) -> NewAuditEvent {
    NewAuditEvent {
        id: Uuid::new_v4().to_string(),
        session_id: None,
        entity_kind: "device".to_owned(),
        entity_id: device_id.to_owned(),
        event_kind: "device_operation_settled".to_owned(),
        summary: format!(
            "Operation {correlation_id} on device '{device_id}' settled: {}",
            outcome.audit_name()
        ),
        metadata: serde_json::json!({
            "correlation_id": correlation_id,
            "outcome": outcome.audit_name(),
        }),
    }
}

async fn record_device_event(
    state: &AppState,
    device_id: &str,
    event_kind: &str,
    summary: String,
    metadata: Value,
) {
    if let Err(error) = state
        .storage
        .create_audit_event(NewAuditEvent {
            id: Uuid::new_v4().to_string(),
            session_id: None,
            entity_kind: "device".to_owned(),
            entity_id: device_id.to_owned(),
            event_kind: event_kind.to_owned(),
            summary,
            metadata,
        })
        .await
    {
        tracing::warn!(%device_id, %event_kind, %error, "failed to record a device audit event");
    }
}

// ---------------------------------------------------------------------------
// The connection a device dials
// ---------------------------------------------------------------------------

/// `GET /api/devices/connect` — the socket a device dials. Authentication
/// is the first frame: a `Hello` whose shared token matches the
/// registration's secret.
pub async fn connect_handler(State(state): State<AppState>, ws: WebSocketUpgrade) -> Response {
    ws.on_upgrade(move |socket| serve_device_link(socket, state))
}

async fn serve_device_link(socket: WebSocket, state: AppState) {
    let (mut sink, mut stream) = socket.split();

    let hello = match tokio::time::timeout(HELLO_TIMEOUT, next_device_frame(&mut stream)).await {
        Ok(Some(DeviceFrame::Hello {
            device_id,
            shared_token,
            label,
            capabilities,
        })) => (device_id, shared_token, label, capabilities),
        Ok(Some(_)) => {
            reject(&mut sink, "the first frame must be a hello").await;
            return;
        }
        Ok(None) => return,
        Err(_) => {
            reject(&mut sink, "no hello within the handshake window").await;
            return;
        }
    };
    let (device_id, shared_token, label, mut capabilities) = hello;

    let device = state.devices.lock().await.devices.get(&device_id).cloned();
    let Some(device) = device else {
        reject(
            &mut sink,
            &format!("device '{device_id}' is not registered"),
        )
        .await;
        return;
    };
    let Some(secret_ref) = device.shared_token_secret_ref.as_deref() else {
        reject(
            &mut sink,
            &format!("device '{device_id}' has no credential"),
        )
        .await;
        return;
    };
    let expected = match state.secret_manager.resolve(secret_ref).await {
        Ok(secret) => secret.value,
        Err(error) => {
            tracing::warn!(%device_id, %error, "device credential could not be resolved");
            reject(&mut sink, "the device's credential could not be resolved").await;
            return;
        }
    };
    if shared_token != expected {
        tracing::warn!(%device_id, "device presented an invalid shared token");
        reject(&mut sink, "invalid shared token").await;
        return;
    }

    // Accepted: record what it can run, hold its link, mark it present.
    capabilities.sort();
    capabilities.dedup();
    let (outbound_tx, mut outbound_rx) = mpsc::unbounded_channel::<GatewayFrame>();
    let link = DeviceLink {
        link_id: Uuid::new_v4(),
        outbound: outbound_tx,
        pending: Arc::new(Mutex::new(HashMap::new())),
        closed: tokio_util::sync::CancellationToken::new(),
    };
    {
        let mut runtime = state.devices.lock().await;
        if let Some(registered) = runtime.devices.get_mut(&device_id) {
            registered.capabilities = capabilities.clone();
            if label.is_some() {
                registered.label = label.clone();
            }
        }
        // A device that dials again replaces its earlier link: the old
        // connection is closed, and its pending operations settle as
        // disconnected when it unwinds.
        if let Some(previous) = runtime.links.insert(device_id.clone(), link.clone()) {
            previous.closed.cancel();
        }
    }
    if let Err(error) = state
        .storage
        .set_device_capabilities(&device_id, capabilities.clone())
        .await
    {
        tracing::warn!(%device_id, %error, "failed to record the device's advertised capabilities");
    }
    crate::presence::upsert_device_heartbeat(
        &state,
        &device_id,
        label.clone().or(device.label.clone()),
        STALE_AFTER_SECS,
        link.link_id,
    )
    .await;
    record_device_event(
        &state,
        &device_id,
        "device_connected",
        format!(
            "Device '{device_id}' connected, advertising {} tool(s)",
            capabilities.len()
        ),
        serde_json::json!({ "capabilities": capabilities, "link_id": link.link_id }),
    )
    .await;

    let forward = tokio::spawn(async move {
        while let Some(frame) = outbound_rx.recv().await {
            let text = match serde_json::to_string(&frame) {
                Ok(text) => text,
                Err(error) => {
                    tracing::error!(%error, "failed to serialise a gateway frame");
                    continue;
                }
            };
            if sink.send(Message::Text(text.into())).await.is_err() {
                break;
            }
        }
        let _ = sink.send(Message::Close(None)).await;
    });
    if link
        .outbound
        .send(GatewayFrame::Welcome {
            device_id: device_id.clone(),
            heartbeat_interval_secs: HEARTBEAT_INTERVAL_SECS,
        })
        .is_err()
    {
        forward.abort();
        return;
    }

    // The gateway going down closes the link: the device's redial loop
    // finds the restarted gateway and heartbeats resume there.
    loop {
        let frame = tokio::select! {
            _ = state.shutdown_signal.cancelled() => break,
            _ = link.closed.cancelled() => break,
            frame = next_device_frame(&mut stream) => match frame {
                Some(frame) => frame,
                None => break,
            },
        };
        match frame {
            DeviceFrame::Heartbeat {} => {
                crate::presence::upsert_device_heartbeat(
                    &state,
                    &device_id,
                    None,
                    STALE_AFTER_SECS,
                    link.link_id,
                )
                .await;
            }
            // A settlement is recorded only where a waiter took it: an
            // answer to an operation the gateway already gave up on
            // (cancelled and recorded by its guard) is not a second
            // settlement.
            DeviceFrame::Result {
                correlation_id,
                output,
            } => {
                let outcome = OperationOutcome::Result(output);
                if link.settle(correlation_id, outcome.clone()) {
                    record_settlement(&state.storage, &device_id, correlation_id, &outcome).await;
                }
            }
            DeviceFrame::Failure {
                correlation_id,
                message,
            } => {
                let outcome = OperationOutcome::Failure(message);
                if link.settle(correlation_id, outcome.clone()) {
                    record_settlement(&state.storage, &device_id, correlation_id, &outcome).await;
                }
            }
            DeviceFrame::Cancelled { correlation_id } => {
                if link.settle(correlation_id, OperationOutcome::Cancelled) {
                    record_settlement(
                        &state.storage,
                        &device_id,
                        correlation_id,
                        &OperationOutcome::Cancelled,
                    )
                    .await;
                }
            }
            DeviceFrame::Hello { .. } => {
                tracing::warn!(%device_id, "ignoring a second hello on an open device connection");
            }
        }
    }

    // The connection ended. Only this link's own entry is removed — a
    // newer dial from the same device has already replaced it.
    forward.abort();
    let still_ours = {
        let mut runtime = state.devices.lock().await;
        match runtime.links.get(&device_id) {
            Some(current) if current.link_id == link.link_id => {
                runtime.links.remove(&device_id);
                true
            }
            _ => false,
        }
    };
    for correlation_id in link.fail_all(OperationOutcome::Disconnected) {
        record_settlement(
            &state.storage,
            &device_id,
            correlation_id,
            &OperationOutcome::Disconnected,
        )
        .await;
    }
    if still_ours {
        crate::presence::disconnect_device(&state, &device_id, Some(link.link_id)).await;
    }
    record_device_event(
        &state,
        &device_id,
        "device_disconnected",
        format!("Device '{device_id}' disconnected"),
        serde_json::json!({ "link_id": link.link_id }),
    )
    .await;
}

/// Close a device's connection from the gateway's side and mark it
/// disconnected. `false` when no such device is registered.
pub async fn disconnect_device(state: &AppState, device_id: &str) -> bool {
    let link = {
        let mut runtime = state.devices.lock().await;
        if !runtime.devices.contains_key(device_id) {
            return false;
        }
        runtime.links.remove(device_id)
    };
    if let Some(link) = link {
        link.closed.cancel();
        for correlation_id in link.fail_all(OperationOutcome::Disconnected) {
            record_settlement(
                &state.storage,
                device_id,
                correlation_id,
                &OperationOutcome::Disconnected,
            )
            .await;
        }
    }
    crate::presence::disconnect_device(state, device_id, None).await;
    true
}

async fn reject(sink: &mut futures::stream::SplitSink<WebSocket, Message>, message: &str) {
    if let Ok(text) = serde_json::to_string(&GatewayFrame::Rejected {
        message: message.to_owned(),
    }) {
        let _ = sink.send(Message::Text(text.into())).await;
    }
    let _ = sink.send(Message::Close(None)).await;
}

async fn next_device_frame(
    stream: &mut futures::stream::SplitStream<WebSocket>,
) -> Option<DeviceFrame> {
    loop {
        match stream.next().await {
            Some(Ok(Message::Text(text))) => match serde_json::from_str::<DeviceFrame>(&text) {
                Ok(frame) => return Some(frame),
                Err(error) => {
                    tracing::warn!(%error, "ignoring a device frame outside the contract");
                }
            },
            Some(Ok(Message::Close(_))) | None => return None,
            Some(Ok(_)) => {}
            Some(Err(error)) => {
                tracing::warn!(%error, "device connection receive error");
                return None;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use swage_storage::SqliteStorage;
    use swage_storage::store::{AuditStore, Storage};
    use swage_tool::context::MockToolContext;

    fn registered_device(device_id: &str, kind: DeviceKind) -> RegisteredDevice {
        RegisteredDevice {
            device_id: device_id.to_string(),
            label: None,
            kind,
            shared_token_secret_ref: Some("secret://device".into()),
            workspace_root: PathBuf::from("/tmp"),
            workspace_roots: vec![PathBuf::from("/tmp")],
            capabilities: vec!["device_identity".into()],
            registered_at: Utc::now(),
        }
    }

    /// A link with no socket behind it: the test reads what the gateway
    /// would send and settles operations as a device would.
    fn open_link(
        runtime: &mut DeviceRuntime,
        device_id: &str,
    ) -> (DeviceLink, mpsc::UnboundedReceiver<GatewayFrame>) {
        let (outbound, inbound) = mpsc::unbounded_channel();
        let link = DeviceLink {
            link_id: Uuid::new_v4(),
            outbound,
            pending: Arc::new(Mutex::new(HashMap::new())),
            closed: tokio_util::sync::CancellationToken::new(),
        };
        runtime.links.insert(device_id.to_owned(), link.clone());
        (link, inbound)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn remote_registration_rejects_a_local_kind_without_a_producer() {
        let storage = Arc::new(SqliteStorage::in_memory().expect("sqlite"));
        let state = crate::test_support::state(storage);
        let error = register_remote_device(
            &state,
            RegisterDeviceRequest {
                device_id: "local-a".into(),
                label: None,
                kind: "local".into(),
                shared_token_secret_ref: "secret://x".into(),
                workspace_root: "/tmp".into(),
                workspace_roots: vec![],
            },
        )
        .await
        .expect_err("local kind is refused");
        assert!(error.to_string().contains("no remote producer"));
        assert!(list_devices(&state).await.devices.is_empty());
    }

    #[test]
    fn select_device_prefers_metadata_backend_kind() {
        let mut devices = HashMap::new();
        devices.insert(
            "ssh-b".to_string(),
            registered_device("ssh-b", DeviceKind::Ssh),
        );
        devices.insert(
            "docker-a".to_string(),
            registered_device("docker-a", DeviceKind::Docker),
        );
        let metadata = serde_json::json!({ "execution_backend": "docker" });
        let selected = select_device(&devices, Some(&metadata)).expect("device");
        assert_eq!(selected.device_id, "docker-a");
    }

    #[test]
    fn select_device_prefers_explicit_device_id_over_backend_kind() {
        let mut devices = HashMap::new();
        devices.insert(
            "ssh-b".to_string(),
            registered_device("ssh-b", DeviceKind::Ssh),
        );
        devices.insert(
            "docker-a".to_string(),
            registered_device("docker-a", DeviceKind::Docker),
        );
        let metadata = serde_json::json!({ "device_id": "ssh-b", "execution_backend": "docker" });
        assert_eq!(
            select_device(&devices, Some(&metadata))
                .expect("device")
                .device_id,
            "ssh-b"
        );
    }

    #[test]
    fn select_device_leaves_local_turns_unrouted_when_only_remote_devices_exist() {
        let mut devices = HashMap::new();
        devices.insert(
            "docker-a".to_string(),
            registered_device("docker-a", DeviceKind::Docker),
        );
        assert!(select_device(&devices, None).is_none());
        assert!(select_device(&devices, Some(&serde_json::json!({}))).is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn execution_refuses_before_the_link_what_the_device_did_not_advertise_or_cannot_reach() {
        let storage = Arc::new(SqliteStorage::in_memory().expect("sqlite"));
        let state = crate::test_support::state(Arc::clone(&storage));
        let device = registered_device("docker-a", DeviceKind::Docker);
        let (_link, mut inbound) = open_link(&mut *state.devices.lock().await, "docker-a");
        let ctx = MockToolContext::new("gateway", "session");

        let unadvertised =
            execute_on_remote_device(&state, &device, "shell", &serde_json::json!({}), &ctx)
                .await
                .expect_err("unadvertised tool");
        assert!(
            unadvertised
                .to_string()
                .contains("does not expose tool 'shell'")
        );

        let mut outside = MockToolContext::new("gateway", "session");
        outside.working_directory = PathBuf::from("/");
        let escaped = execute_on_remote_device(
            &state,
            &device,
            "device_identity",
            &serde_json::json!({}),
            &outside,
        )
        .await
        .expect_err("context outside the workspace");
        assert!(escaped.to_string().contains("escapes device workspace"));

        assert!(inbound.try_recv().is_err(), "nothing reached the link");
        assert!(
            storage.list_audit_events().await.expect("audit").is_empty(),
            "a refused operation is not an invocation"
        );

        let unconnected = registered_device("docker-b", DeviceKind::Docker);
        let not_connected = execute_on_remote_device(
            &state,
            &unconnected,
            "device_identity",
            &serde_json::json!({}),
            &ctx,
        )
        .await
        .expect_err("device without a link");
        assert!(not_connected.to_string().contains("is not connected"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_operation_travels_the_link_with_one_correlation_id_and_settles_into_the_audit_trail()
     {
        let storage = Arc::new(SqliteStorage::in_memory().expect("sqlite"));
        let state = crate::test_support::state(Arc::clone(&storage));
        let device = registered_device("docker-a", DeviceKind::Docker);
        let (link, mut inbound) = open_link(&mut *state.devices.lock().await, "docker-a");
        let ctx = MockToolContext::new("gateway", "session");

        let invocation = tokio::spawn({
            let state = state.clone();
            let device = device.clone();
            async move {
                execute_on_remote_device(
                    &state,
                    &device,
                    "device_identity",
                    &serde_json::json!({ "n": 1 }),
                    &MockToolContext::new("gateway", "session"),
                )
                .await
            }
        });
        let frame = inbound.recv().await.expect("operation frame");
        let GatewayFrame::Operation {
            correlation_id,
            tool_name,
            params,
            context,
        } = frame
        else {
            panic!("expected an operation frame");
        };
        assert_eq!(tool_name, "device_identity");
        assert_eq!(params, serde_json::json!({ "n": 1 }));
        assert_eq!(context.session_id, ctx.session_id());

        // The device's answer settles the waiter and is recorded where it
        // is received, as the link loop does.
        let outcome = OperationOutcome::Result(serde_json::json!({ "device": "docker-a" }));
        assert!(link.settle(correlation_id, outcome.clone()));
        record_settlement(
            &(Arc::clone(&storage) as Arc<dyn Storage>),
            "docker-a",
            correlation_id,
            &outcome,
        )
        .await;
        let output = invocation.await.expect("join").expect("result");
        assert_eq!(output["device"], "docker-a");
        // A second answer for the same operation has no waiter and is not
        // a settlement.
        assert!(!link.settle(correlation_id, outcome));

        // The listing is newest-first; read it in the order things happened.
        let mut events = storage.list_audit_events().await.expect("audit");
        events.sort_by_key(|event| event.created_at);
        let kinds = events
            .iter()
            .map(|event| event.event_kind.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            kinds,
            ["device_operation_invoked", "device_operation_settled"]
        );
        assert!(events.iter().all(|event| {
            event.entity_id == "docker-a"
                && event.metadata["correlation_id"] == serde_json::json!(correlation_id)
        }));
        assert_eq!(events[1].metadata["outcome"], "result");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_future_dropped_before_its_operation_frame_is_sent_still_cancels_and_records() {
        let storage = Arc::new(SqliteStorage::in_memory().expect("sqlite"));
        let state = crate::test_support::state(Arc::clone(&storage));
        let device = registered_device("docker-a", DeviceKind::Docker);
        let (link, mut inbound) = open_link(&mut *state.devices.lock().await, "docker-a");
        let ctx = MockToolContext::new("gateway", "session");
        let params = serde_json::json!({});

        // Poll the invocation exactly once: it registers as pending and
        // parks on the audit write, then is dropped before the operation
        // frame goes out.
        let mut invocation = Box::pin(execute_on_remote_device(
            &state,
            &device,
            "device_identity",
            &params,
            &ctx,
        ));
        let waker = futures::task::noop_waker();
        let mut cx = std::task::Context::from_waker(&waker);
        assert!(std::future::Future::poll(invocation.as_mut(), &mut cx).is_pending());
        assert_eq!(link.pending.lock().expect("lock").len(), 1);
        drop(invocation);

        assert!(link.pending.lock().expect("lock").is_empty());
        // Whether or not the operation frame had already left (an in-memory
        // store may not yield during the audit write), the drop sends the
        // cancel as its last word.
        let mut frames = Vec::new();
        while let Ok(frame) = inbound.try_recv() {
            frames.push(frame);
        }
        assert!(
            matches!(frames.last(), Some(GatewayFrame::Cancel { .. })),
            "expected a cancel frame last, got {frames:?}"
        );
        let settled = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let events = storage.list_audit_events().await.expect("audit");
                if let Some(event) = events
                    .iter()
                    .find(|event| event.event_kind == "device_operation_settled")
                {
                    return event.clone();
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the cancellation is recorded");
        assert_eq!(settled.metadata["outcome"], "cancelled");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn dropping_an_unsettled_operation_cancels_it_on_the_device_and_records_it() {
        let storage = Arc::new(SqliteStorage::in_memory().expect("sqlite"));
        let state = crate::test_support::state(Arc::clone(&storage));
        let device = registered_device("docker-a", DeviceKind::Docker);
        let (link, mut inbound) = open_link(&mut *state.devices.lock().await, "docker-a");

        let invocation = tokio::spawn({
            let state = state.clone();
            let device = device.clone();
            async move {
                execute_on_remote_device(
                    &state,
                    &device,
                    "device_identity",
                    &serde_json::json!({}),
                    &MockToolContext::new("gateway", "session"),
                )
                .await
            }
        });
        let Some(GatewayFrame::Operation { correlation_id, .. }) = inbound.recv().await else {
            panic!("expected an operation frame");
        };

        // The turn is cancelled: its future is dropped mid-await.
        invocation.abort();
        let cancel = tokio::time::timeout(Duration::from_secs(2), inbound.recv())
            .await
            .expect("a cancel frame in time")
            .expect("frame");
        assert_eq!(cancel, GatewayFrame::Cancel { correlation_id });
        assert!(
            link.pending.lock().expect("lock").is_empty(),
            "the cancelled operation no longer waits"
        );

        let settled = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let events = storage.list_audit_events().await.expect("audit");
                if let Some(event) = events
                    .iter()
                    .find(|event| event.event_kind == "device_operation_settled")
                {
                    return event.clone();
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the cancellation is recorded");
        assert_eq!(settled.metadata["outcome"], "cancelled");
        assert_eq!(
            settled.metadata["correlation_id"],
            serde_json::json!(correlation_id)
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn disconnecting_a_device_fails_its_pending_operations_as_disconnected() {
        let storage = Arc::new(SqliteStorage::in_memory().expect("sqlite"));
        let state = crate::test_support::state(storage);
        let device = registered_device("docker-a", DeviceKind::Docker);
        let (_link, mut inbound) = {
            let mut runtime = state.devices.lock().await;
            runtime
                .devices
                .insert(device.device_id.clone(), device.clone());
            open_link(&mut runtime, "docker-a")
        };

        let invocation = tokio::spawn({
            let state = state.clone();
            let device = device.clone();
            async move {
                execute_on_remote_device(
                    &state,
                    &device,
                    "device_identity",
                    &serde_json::json!({}),
                    &MockToolContext::new("gateway", "session"),
                )
                .await
            }
        });
        let _ = inbound.recv().await.expect("operation frame");
        assert!(disconnect_device(&state, "docker-a").await);
        assert!(state.devices.lock().await.links.is_empty());
        let error = invocation.await.expect("join").expect_err("disconnected");
        assert!(error.to_string().contains("disconnected"));
        assert!(!disconnect_device(&state, "unknown").await);
    }
}

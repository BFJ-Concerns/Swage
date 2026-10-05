//! Host-side ownership of a tool call while its result is pending.
//!
//! A turn is cancelled by dropping the future that waits on its tool call.
//! Without help, the sandboxed agent would never learn of it and the call's
//! command would run on. [`InFlightToolCall`] holds the transport for the
//! call's duration; if it is dropped before the result arrives it sends
//! `ToolCallCancel` while still holding the transport, so the cancel always
//! precedes any later request on the same connection.

use tokio::sync::OwnedMutexGuard;

use crate::ipc::message::IpcMessage;
use crate::ipc::transport::IpcTransport;

/// Exclusive use of the transport for one dispatched tool call.
pub(crate) struct InFlightToolCall {
    /// `None` once the call has settled; the guard then releases normally.
    transport: Option<OwnedMutexGuard<Option<IpcTransport>>>,
    request_id: String,
}

impl InFlightToolCall {
    /// Take ownership of `transport` (which must hold a connection) for the
    /// call identified by `request_id`.
    pub(crate) fn new(
        transport: OwnedMutexGuard<Option<IpcTransport>>,
        request_id: String,
    ) -> Self {
        debug_assert!(transport.is_some(), "in-flight call needs a connection");
        Self {
            transport: Some(transport),
            request_id,
        }
    }

    pub(crate) fn request_id(&self) -> &str {
        &self.request_id
    }

    pub(crate) fn transport(&mut self) -> &mut IpcTransport {
        self.transport
            .as_mut()
            .and_then(|guard| guard.as_mut())
            .expect("an in-flight call holds its connection until it settles")
    }

    /// The call reached its result — dropping it now sends no cancel.
    pub(crate) fn settle(mut self) {
        self.transport = None;
    }
}

impl Drop for InFlightToolCall {
    fn drop(&mut self) {
        let Some(mut guard) = self.transport.take() else {
            return;
        };
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let request_id = std::mem::take(&mut self.request_id);
        // The guard moves into the task, so no other call can use the
        // connection until the cancel has been written.
        runtime.spawn(async move {
            let Some(transport) = guard.as_mut() else {
                return;
            };
            transport.abandon_tool_call(request_id.clone());
            if let Err(error) = transport
                .send(&IpcMessage::ToolCallCancel { request_id })
                .await
            {
                tracing::warn!("failed to cancel abandoned sandbox tool call: {error}");
            }
        });
    }
}

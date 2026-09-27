//! The ways a caller starts a turn.
//!
//! Placed here because it has one responsibility a reader can hold without
//! the coordinator: each entry point is one caller's shape of request —
//! a plain submission, an existing turn, a compaction focus, a requested
//! model, a dispatch composition — and supplies the defaults for every
//! option its caller does not carry before funnelling into the one
//! coordinator, `process_turn_with_composition_and_focus`.

use super::{
    AppState, Arc, CancellationToken, DispatchContextComposition, GatewayError, Mutex,
    ProviderFailureObserver, ServerEvent, SubscriptionState, TurnAttendance, TurnInput, Uuid, mpsc,
    process_turn_with_composition_and_focus,
};

/// Orchestrate a complete agent turn: user message → provider stream → storage.
///
/// # Flow
/// 1. Look up the session's default thread (created alongside the session).
/// 2. Guard: reject if a turn is already Processing for this thread.
/// 3. Resolve the topic for the session; load per-topic config and build ToolContext.
/// 4. Create a Turn record (state=Pending), immediately advance to Processing.
/// 5. Store the user message with thread_id and position.
/// 6. Build message history from the thread and send a streaming request to the provider.
/// 7. For each chunk: forward Token events to the WebSocket, accumulate content.
/// 8. On Done: store the assistant message, mark the turn Completed, send TurnComplete.
/// 9. On Cancel: store any partial response, mark the turn Interrupted, send Cancelled.
/// 10. On provider Error: mark the turn Failed, send an Error event.
/// 11. Cleanup: remove the CancellationToken from active_turns.
pub async fn process_turn(
    state: &AppState,
    session_id: Uuid,
    target_thread_id: Option<Uuid>,
    input: impl Into<TurnInput>,
    ws_tx: mpsc::UnboundedSender<ServerEvent>,
    subscription: Arc<Mutex<SubscriptionState>>,
    output_schema_override: Option<serde_json::Value>,
) -> Result<(), GatewayError> {
    process_turn_with_existing_turn(
        state,
        session_id,
        target_thread_id,
        input.into(),
        ws_tx,
        subscription,
        output_schema_override,
        None,
        None,
        None,
        None,
        TurnAttendance::Attended,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn process_turn_with_existing_turn(
    state: &AppState,
    session_id: Uuid,
    target_thread_id: Option<Uuid>,
    input: impl Into<TurnInput>,
    ws_tx: mpsc::UnboundedSender<ServerEvent>,
    subscription: Arc<Mutex<SubscriptionState>>,
    output_schema_override: Option<serde_json::Value>,
    max_output_retries_override: Option<u32>,
    effort_override: Option<swage_core::Effort>,
    cancellation_override: Option<CancellationToken>,
    existing_turn_id: Option<Uuid>,
    attendance: TurnAttendance,
) -> Result<(), GatewayError> {
    process_turn_with_existing_turn_and_focus(
        state,
        session_id,
        target_thread_id,
        input.into(),
        ws_tx,
        subscription,
        output_schema_override,
        max_output_retries_override,
        effort_override,
        cancellation_override,
        existing_turn_id,
        attendance,
        None,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn process_turn_with_existing_turn_and_focus(
    state: &AppState,
    session_id: Uuid,
    target_thread_id: Option<Uuid>,
    input: impl Into<TurnInput>,
    ws_tx: mpsc::UnboundedSender<ServerEvent>,
    subscription: Arc<Mutex<SubscriptionState>>,
    output_schema_override: Option<serde_json::Value>,
    max_output_retries_override: Option<u32>,
    effort_override: Option<swage_core::Effort>,
    cancellation_override: Option<CancellationToken>,
    existing_turn_id: Option<Uuid>,
    attendance: TurnAttendance,
    compaction_focus: Option<String>,
) -> Result<(), GatewayError> {
    process_turn_with_existing_turn_and_focus_and_model(
        state,
        session_id,
        target_thread_id,
        input.into(),
        ws_tx,
        subscription,
        output_schema_override,
        max_output_retries_override,
        effort_override,
        cancellation_override,
        existing_turn_id,
        attendance,
        compaction_focus,
        None,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn process_turn_with_existing_turn_and_focus_and_model(
    state: &AppState,
    session_id: Uuid,
    target_thread_id: Option<Uuid>,
    input: impl Into<TurnInput>,
    ws_tx: mpsc::UnboundedSender<ServerEvent>,
    subscription: Arc<Mutex<SubscriptionState>>,
    output_schema_override: Option<serde_json::Value>,
    max_output_retries_override: Option<u32>,
    effort_override: Option<swage_core::Effort>,
    cancellation_override: Option<CancellationToken>,
    existing_turn_id: Option<Uuid>,
    attendance: TurnAttendance,
    compaction_focus: Option<String>,
    requested_model: Option<String>,
) -> Result<(), GatewayError> {
    process_turn_with_composition_and_focus(
        state,
        session_id,
        target_thread_id,
        input.into(),
        ws_tx,
        subscription,
        output_schema_override,
        max_output_retries_override,
        effort_override,
        cancellation_override,
        existing_turn_id,
        attendance,
        DispatchContextComposition::default(),
        None,
        compaction_focus,
        requested_model,
        None,
        false,
    )
    .await
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub(crate) async fn process_turn_with_composition(
    state: &AppState,
    session_id: Uuid,
    target_thread_id: Option<Uuid>,
    input: impl Into<TurnInput>,
    ws_tx: mpsc::UnboundedSender<ServerEvent>,
    subscription: Arc<Mutex<SubscriptionState>>,
    output_schema_override: Option<serde_json::Value>,
    max_output_retries_override: Option<u32>,
    effort_override: Option<swage_core::Effort>,
    cancellation_override: Option<CancellationToken>,
    existing_turn_id: Option<Uuid>,
    attendance: TurnAttendance,
    composition: DispatchContextComposition,
    agent_call_scope: Option<Uuid>,
) -> Result<(), GatewayError> {
    process_turn_with_composition_and_focus(
        state,
        session_id,
        target_thread_id,
        input.into(),
        ws_tx,
        subscription,
        output_schema_override,
        max_output_retries_override,
        effort_override,
        cancellation_override,
        existing_turn_id,
        attendance,
        composition,
        agent_call_scope,
        None,
        None,
        None,
        false,
    )
    .await
}

/// Process a composed turn with a call-scoped model selection. Workflow calls
/// use this to make their journalled model choice explicit without changing
/// the session's durable selection for an ordinary one-off turn.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn process_turn_with_composition_and_model(
    state: &AppState,
    session_id: Uuid,
    target_thread_id: Option<Uuid>,
    input: impl Into<TurnInput>,
    ws_tx: mpsc::UnboundedSender<ServerEvent>,
    subscription: Arc<Mutex<SubscriptionState>>,
    output_schema_override: Option<serde_json::Value>,
    max_output_retries_override: Option<u32>,
    effort_override: Option<swage_core::Effort>,
    cancellation_override: Option<CancellationToken>,
    existing_turn_id: Option<Uuid>,
    attendance: TurnAttendance,
    composition: DispatchContextComposition,
    agent_call_scope: Option<Uuid>,
    requested_model: Option<String>,
    provider_failure_observer: Option<ProviderFailureObserver>,
    reuse_persisted_input: bool,
) -> Result<(), GatewayError> {
    process_turn_with_composition_and_focus(
        state,
        session_id,
        target_thread_id,
        input.into(),
        ws_tx,
        subscription,
        output_schema_override,
        max_output_retries_override,
        effort_override,
        cancellation_override,
        existing_turn_id,
        attendance,
        composition,
        agent_call_scope,
        None,
        requested_model,
        provider_failure_observer,
        reuse_persisted_input,
    )
    .await
}

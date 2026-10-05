//! Releasing the turn's active-turn guard, and the records a turn writes on
//! its way out before the agent has taken it over.
//!
//! Placed here because it has one responsibility a reader can hold without
//! the coordinator: every pre-agent failure path marks the turn terminal and
//! frees the session's active-turn slot the same way, and the agent-call
//! attempt is recorded before either runtime may execute. The coordinator
//! decides when a path fails; these helpers are how it fails.
//!
//! The turn-state writes and the agent-call attempt are constitutive records
//! (record_durability.rs): each propagates its storage error.

use super::{
    AppState, GatewayError, SandboxGuard, TurnState, Uuid, WorkflowAgentCallResolution,
    shutdown_sandbox_guard,
};

/// Keep the session occupied until the drained turn's buffered events have
/// entered its listeners, on every terminal path.
pub(super) async fn release_active_turn(
    state: &AppState,
    session_id: Uuid,
    turn_id: Uuid,
) -> Result<(), GatewayError> {
    let delivery = state
        .turn_event_deliveries
        .lock()
        .await
        .get(&turn_id)
        .cloned();
    if let Some(delivery) = delivery {
        // Settlement serialises releases for this turn without holding the registry.
        let mut delivery = delivery.lock().await;
        let Some(relay) = delivery.take() else {
            return Ok(());
        };
        let result = relay.finish().await;
        state.active_turns.lock().await.remove(&session_id);
        state.turn_event_deliveries.lock().await.remove(&turn_id);
        result
    } else {
        state.active_turns.lock().await.remove(&session_id);
        Ok(())
    }
}

/// Record the turn's terminal state and release the session. Every path a
/// turn ends on — before or after its agent ran — comes through here, so
/// this is also where a session is handed to its queue: a turn that fails
/// at settlement drains what waited behind it as a completed one does.
pub(crate) async fn record_terminal_state_and_release(
    state: &AppState,
    session_id: Uuid,
    turn_id: Uuid,
    turn_state: TurnState,
) -> Result<(), GatewayError> {
    let drains_queue = crate::turn_queue::drains_queue_after(turn_state.clone());
    let recorded = state.storage.update_turn_state(turn_id, turn_state).await;
    release_active_turn(state, session_id, turn_id).await?;
    recorded?;
    if drains_queue {
        crate::turn_queue::spawn_drain(state.clone(), session_id);
    }
    Ok(())
}

/// Persist the resolved dispatch before a provider can execute it.
///
/// Both provider runtimes share this refusal path so neither can make a call
/// whose workflow attempt cannot be reconstructed from the durable record.
pub(super) async fn record_agent_call_attempt_before_execution(
    state: &AppState,
    session_id: Uuid,
    prepared_turn_id: Uuid,
    agent_call_scope: Option<Uuid>,
    resolution: &WorkflowAgentCallResolution,
    sandbox_guard: &Option<SandboxGuard>,
) -> Result<(), GatewayError> {
    let Some(agent_call_scope) = agent_call_scope else {
        return Ok(());
    };
    let result = match serde_json::to_value(resolution) {
        Ok(resolution) => crate::workflow::record_agent_call_attempt(
            state.storage.as_ref(),
            agent_call_scope,
            resolution,
        )
        .await
        .map_err(|error| {
            GatewayError::WebSocketError(format!("failed to record agent call attempt: {error}"))
        }),
        Err(error) => Err(GatewayError::WebSocketError(format!(
            "agent call attempt could not be serialised: {error}"
        ))),
    };
    if let Err(error) = result {
        shutdown_sandbox_guard(
            sandbox_guard,
            "failed to shut down sandbox after agent call attempt recording failure",
        )
        .await;
        record_terminal_state_and_release(state, session_id, prepared_turn_id, TurnState::Failed)
            .await?;
        return Err(error);
    }
    Ok(())
}

pub(super) async fn record_failed_turn_and_release_on_error<T>(
    state: &AppState,
    session_id: Uuid,
    turn_id: Uuid,
    result: Result<T, GatewayError>,
) -> Result<T, GatewayError> {
    match result {
        Ok(value) => Ok(value),
        Err(error) => {
            record_terminal_state_and_release(state, session_id, turn_id, TurnState::Failed)
                .await?;
            Err(error)
        }
    }
}

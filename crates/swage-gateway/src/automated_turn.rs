use std::sync::Arc;

use tokio::sync::{Mutex, mpsc};
use uuid::Uuid;

use swage_core::GatewayError;
use swage_storage::NewTurn;

use crate::state::AppState;
use crate::subscription::SubscriptionState;
use crate::thread_selection::resolve_thread_for_session;

pub async fn create_placeholder_turn(
    state: &AppState,
    session_id: Uuid,
    agent_id: &str,
    message: &str,
) -> Result<Uuid, GatewayError> {
    let thread = resolve_thread_for_session(state.storage.as_ref(), session_id, None).await?;
    let existing_turns = state.storage.list_turns_by_thread(thread.id).await?;
    if existing_turns.iter().any(|turn| {
        matches!(
            turn.state,
            swage_storage::TurnState::Pending
                | swage_storage::TurnState::Processing
                | swage_storage::TurnState::AwaitingApproval
                | swage_storage::TurnState::Delegating
        )
    }) || state.active_turns.lock().await.contains_key(&session_id)
    {
        return Err(GatewayError::TurnInProgress { session_id });
    }

    let turn = state
        .storage
        .create_turn(NewTurn {
            thread_id: thread.id,
            agent_id: agent_id.to_string(),
            state: swage_storage::TurnState::Pending,
            user_input: Some(message.to_string()),
            parent_turn_id: None,
        })
        .await?;
    Ok(turn.id)
}

pub async fn run_placeholder_turn(
    state: &AppState,
    session_id: Uuid,
    message: &str,
    turn_id: Uuid,
) -> Result<String, GatewayError> {
    let thread = resolve_thread_for_session(state.storage.as_ref(), session_id, None).await?;
    let (ws_tx, mut ws_rx) = mpsc::unbounded_channel();
    let subscription = Arc::new(Mutex::new(SubscriptionState::new(false)));
    tokio::spawn(async move { while ws_rx.recv().await.is_some() {} });

    crate::turn::process_turn_with_existing_turn(
        state,
        session_id,
        Some(thread.id),
        message.to_string(),
        ws_tx,
        subscription,
        None,
        Some(turn_id),
    )
    .await?;

    let turn = state.storage.get_turn(turn_id).await?.ok_or_else(|| {
        GatewayError::ValidationError(format!(
            "turn {turn_id} not found after automated execution"
        ))
    })?;
    turn.assistant_response.ok_or_else(|| {
        GatewayError::ValidationError("automated turn completed without assistant response".into())
    })
}

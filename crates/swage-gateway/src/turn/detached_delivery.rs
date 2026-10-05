//! Delivery of completed detached delegations into the parent's thread.
//!
//! Placed here because it has one responsibility a reader can hold without
//! the coordinator: carrying each detached child's Result from this agent's
//! mailbox into the thread record, append-before-claim, before history is
//! rebuilt. Failing the turn when delivery fails stays the coordinator's.

use swage_storage::MessageOrigin;

use super::{AppState, GatewayError, Uuid};
use crate::service::{PlatformMessage, append_platform_message};

/// Deliver completed detached delegations. A detached child's
/// Result waits in this agent's mailbox until its parent next turns;
/// each is appended as a durable platform message of origin
/// `delegation` *before* history is rebuilt, so the result enters the
/// thread record — labelled by its origin in the UI, announced to open
/// views, part of the audit trail, and carried to the model as ordinary
/// history content, never as instructions. User role, deliberately:
/// the history rebuild carries Tool-role rows only when they answer a
/// recorded tool call (providers reject orphaned tool results), so a
/// platform-delivered notification rides the same role a channel or
/// scheduled message would, with the recorded origin naming its real sender.
/// Append-before-claim: a failure between the two re-delivers next
/// turn rather than losing the result, and a lost claim race leaves a
/// duplicate row, not a gap.
pub(super) async fn deliver_detached_results(
    state: &AppState,
    session_id: Uuid,
    agent_id: &str,
    thread_id: Uuid,
    prepared_turn_id: Uuid,
) -> Result<(), GatewayError> {
    let pending_results = swage_agent::mailbox::pending_results(&state.storage, agent_id)
        .await
        .map_err(|error| GatewayError::WebSocketError(error.to_string()))?;
    for result in &pending_results {
        append_platform_message(
            state,
            PlatformMessage {
                session_id,
                thread_id,
                turn_id: Some(prepared_turn_id),
                agent_id: agent_id.to_owned(),
                origin: MessageOrigin::Delegation,
                content: render_delegation_result(result),
            },
        )
        .await?;
        swage_agent::mailbox::claim_result(&state.storage, result.id)
            .await
            .map_err(|error| GatewayError::WebSocketError(error.to_string()))?;
    }
    Ok(())
}

/// Render a completed delegation's Result message as thread content.
///
/// Reads the payload the delegation path writes — `response` with optional
/// `structured_output`, or `error` for a failed child — and falls back to
/// the raw JSON for any other shape, so an unrecognised payload is still
/// delivered rather than dropped.
pub(super) fn render_delegation_result(result: &swage_storage::model::AgentMessage) -> String {
    let sender = &result.sender_agent_id;
    let mut text = match result.payload.get("response").and_then(|v| v.as_str()) {
        Some(response) => match result
            .payload
            .get("incomplete_reason")
            .and_then(|v| v.as_str())
        {
            Some(reason) => format!("Delegated agent {sender} incomplete ({reason}):\n{response}"),
            None => format!("Delegated agent {sender} completed:\n{response}"),
        },
        None => match result.payload.get("error").and_then(|v| v.as_str()) {
            Some(error) => format!("Delegated agent {sender} failed: {error}"),
            None => format!(
                "Delegated agent {sender} finished:\n{}",
                serde_json::to_string_pretty(&result.payload)
                    .unwrap_or_else(|_| result.payload.to_string())
            ),
        },
    };
    if let Some(structured) = result.payload.get("structured_output") {
        let rendered =
            serde_json::to_string_pretty(structured).unwrap_or_else(|_| structured.to_string());
        text.push_str("\n\nStructured output:\n```json\n");
        text.push_str(&rendered);
        text.push_str("\n```");
    }
    text
}

//! Delivery of completed detached delegations into the parent's thread.
//!
//! Placed here because it has one responsibility a reader can hold without
//! the coordinator: carrying each detached child's Result from this agent's
//! mailbox into the thread record, append-before-claim, before history is
//! rebuilt. Failing the turn when delivery fails stays the coordinator's.

use swage_agent::AgentError;
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
///
/// Each Result is put to the message router's delivery hooks first, as a
/// Result the synchronous path hands straight to its parent is: a hook
/// that blocks it ends it — the router marks it failed with the reason,
/// nothing of it enters the thread, and the thread is told the result was
/// blocked, so the parent knows the child answered and why it cannot read
/// the answer.
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
    // The mailbox is the agent's, and an agent may run several sessions,
    // each with several threads: a Result that names the parent session
    // and thread it belongs to (recovery writes one so) is delivered only
    // there; one that names none is the agent's to take in whichever of
    // its sessions turns next.
    let here_session = session_id.to_string();
    let here_thread = thread_id.to_string();
    let addressed_here = |result: &swage_storage::model::AgentMessage| {
        let named = |key: &str| result.payload.get(key).and_then(|value| value.as_str());
        named("parent_session_id").is_none_or(|parent| parent == here_session)
            && named("parent_thread_id").is_none_or(|parent| parent == here_thread)
    };
    let router = state.message_router();
    for result in pending_results
        .iter()
        .filter(|result| addressed_here(result))
    {
        let content = match router.check_delivery(result).await {
            Ok(()) => render_delegation_result(result),
            Err(AgentError::HookBlocked { reason, .. }) => format!(
                "Delegated agent {}'s result was blocked before delivery ({reason}).",
                result.sender_agent_id
            ),
            Err(error) => return Err(GatewayError::WebSocketError(error.to_string())),
        };
        append_platform_message(
            state,
            PlatformMessage {
                session_id,
                thread_id,
                turn_id: Some(prepared_turn_id),
                agent_id: agent_id.to_owned(),
                origin: MessageOrigin::Delegation,
                content,
            },
        )
        .await?;
        // A blocked Result is already failed by the router and no longer
        // pending; the claim is a no-op for it and marks a passed one
        // delivered.
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
    let text_of = |key: &str| result.payload.get(key).and_then(|v| v.as_str());
    // A fate is read from the payload's error first: a refusal or a
    // failure may carry the text the child produced, and that text does
    // not make it a completion.
    let mut text = match (text_of("error"), text_of("response")) {
        (Some(error), response) => {
            let refused = result.payload.get("refused").and_then(|v| v.as_bool()) == Some(true);
            let heading = if refused {
                format!("Delegated agent {sender} refused the task")
            } else {
                format!("Delegated agent {sender} failed: {error}")
            };
            match response.filter(|text| !text.is_empty()) {
                Some(response) => format!("{heading}:\n{response}"),
                None => heading,
            }
        }
        (None, Some(response)) => match text_of("incomplete_reason") {
            Some(reason) => format!("Delegated agent {sender} incomplete ({reason}):\n{response}"),
            None => format!("Delegated agent {sender} completed:\n{response}"),
        },
        (None, None) => format!(
            "Delegated agent {sender} finished:\n{}",
            serde_json::to_string_pretty(&result.payload)
                .unwrap_or_else(|_| result.payload.to_string())
        ),
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

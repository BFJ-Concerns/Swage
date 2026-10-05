//! A message the platform puts into a conversation — a schedule's brief, a
//! delegation's completion notice, a notice about the operator's memory —
//! recorded with the origin that composed it and announced to every client
//! holding the conversation open.
//!
//! Placed here because it is the one route for a row a person did not type:
//! persistence and announcement travel together, so a platform message never
//! reaches a view only on reload, and the author label a person reads is
//! keyed on a recorded fact rather than guessed from the provider-facing
//! role. The person's own messages take the turn path and reach their view
//! as they type; the model's replies stream. Neither comes through here.

use swage_core::{GatewayError, Role};
use swage_protocol::client::conversation::{MessageView, TurnState as WireTurnState};
use swage_protocol::client::websocket::ServerEvent;
use swage_storage::{MessageOrigin, model::AppendMessage};
use uuid::Uuid;

use crate::state::AppState;

use super::conversation::recorded_message_view;

/// What the platform is putting into the conversation.
#[derive(Debug, Clone)]
pub(crate) struct PlatformMessage {
    pub session_id: Uuid,
    pub thread_id: Uuid,
    /// The turn the message belongs to — a brief that opens a turn, a
    /// notice delivered as a turn begins — or none for a notice attached to
    /// an idle conversation.
    pub turn_id: Option<Uuid>,
    /// The agent the conversation runs under; the row records it like any
    /// other message the agent receives.
    pub agent_id: String,
    /// Who composed it. Never `User`, `Assistant` or `Tool`: those rows have
    /// their own routes, and routing one through here would announce a
    /// message its author's client already holds.
    pub origin: MessageOrigin,
    pub content: String,
}

/// Record a platform message on its thread and announce it to the
/// conversation's open views. The row rides the user role, so the model
/// treats a brief or notice as material addressed to it — the same role a
/// channel or typed message carries — while the recorded origin keeps the
/// author label truthful.
pub(crate) async fn append_platform_message(
    state: &AppState,
    message: PlatformMessage,
) -> Result<MessageView, GatewayError> {
    if matches!(
        message.origin,
        MessageOrigin::User | MessageOrigin::Assistant | MessageOrigin::Tool
    ) {
        return Err(GatewayError::ValidationError(format!(
            "a message of origin '{}' is not a platform message",
            message.origin
        )));
    }

    let recorded = state
        .storage
        .append_message(AppendMessage {
            thread_id: message.thread_id,
            turn_id: message.turn_id,
            role: Role::User,
            origin: message.origin,
            content: message.content,
            attachments: Vec::new(),
            agent_id: message.agent_id,
        })
        .await?;

    let view = recorded_message_view(state.storage.as_ref(), recorded).await?;
    announce_appended_message(state, message.session_id, view.clone());
    Ok(view)
}

/// Tell every client with the conversation open that a recorded message
/// joined it. Delivery is best effort over the server-wide broadcast: a
/// client with no connection open reads the row when it next loads the
/// conversation, which is why the record is written first.
pub(crate) fn announce_appended_message(state: &AppState, session_id: Uuid, message: MessageView) {
    let _ = state.broadcast.send(ServerEvent::MessageAppended {
        session_id,
        message,
    });
}

/// The wire state an inbound row's turn holds the moment it is recorded.
pub(crate) fn pending_turn_state() -> Option<WireTurnState> {
    Some(WireTurnState::Pending)
}

#[cfg(test)]
mod tests {
    use swage_core::Role;
    use swage_protocol::client::websocket::ServerEvent;
    use swage_storage::MessageOrigin;

    use super::*;
    use crate::thread_selection::resolve_thread_for_session;
    use crate::turn::tests::make_state_with_session;

    #[tokio::test(flavor = "multi_thread")]
    async fn a_platform_message_is_recorded_by_its_origin_and_announced_to_its_conversation() {
        let (state, session_id) = make_state_with_session(Vec::new()).await;
        let thread = resolve_thread_for_session(state.storage.as_ref(), session_id, None)
            .await
            .expect("thread");
        let mut announcements = state.broadcast.subscribe();

        let view = append_platform_message(
            &state,
            PlatformMessage {
                session_id,
                thread_id: thread.id,
                turn_id: None,
                agent_id: "gateway".into(),
                origin: MessageOrigin::Delegation,
                content: "Delegated agent worker completed:\ndone".into(),
            },
        )
        .await
        .expect("platform message");

        // Recorded first: the row is there whether or not anyone is listening.
        let recorded = state
            .storage
            .list_messages_by_thread(thread.id)
            .await
            .expect("messages");
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].role, Role::User);
        assert_eq!(recorded[0].origin, MessageOrigin::Delegation);
        assert_eq!(recorded[0].id, view.id);
        assert_eq!(view.origin, MessageOrigin::Delegation);
        assert_eq!(
            view.turn_state, None,
            "a notice with no turn reports no turn state"
        );

        // Then announced, as the same view the conversation would load.
        match announcements.try_recv().expect("announced") {
            ServerEvent::MessageAppended {
                session_id: announced,
                message,
            } => {
                assert_eq!(announced, session_id);
                assert_eq!(message.id, view.id);
                assert_eq!(message.origin, MessageOrigin::Delegation);
            }
            other => panic!("expected the announcement, got {other:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_typed_or_model_message_is_refused_as_a_platform_message() {
        // Those rows have their own routes and their authors' clients already
        // hold them; routing one here would record nothing and announce a
        // duplicate.
        let (state, session_id) = make_state_with_session(Vec::new()).await;
        let thread = resolve_thread_for_session(state.storage.as_ref(), session_id, None)
            .await
            .expect("thread");
        for origin in [
            MessageOrigin::User,
            MessageOrigin::Assistant,
            MessageOrigin::Tool,
        ] {
            let error = append_platform_message(
                &state,
                PlatformMessage {
                    session_id,
                    thread_id: thread.id,
                    turn_id: None,
                    agent_id: "gateway".into(),
                    origin,
                    content: "not a platform message".into(),
                },
            )
            .await
            .expect_err("refused");
            assert!(
                matches!(error, GatewayError::ValidationError(ref message) if message.contains(origin.as_str())),
                "{origin}: {error}"
            );
        }
        let recorded = state
            .storage
            .list_messages_by_thread(thread.id)
            .await
            .expect("messages");
        assert!(recorded.is_empty(), "a refused message leaves no row");
    }
}

//! Messages that arrive while a session's turn is running.
//!
//! A person's message is never refused for arriving mid-turn (C60): it is
//! accepted, held durably in arrival order, shown as queued to every open
//! view, and becomes the next turn's input the moment the running turn ends
//! — one turn, every queued message in it, each recorded as its own message
//! with the origin it arrived with. Stopping a turn leaves the queue intact:
//! the person decides what happens next. A gateway restart drains any queue
//! it finds once the orphaned turns are settled. One queue serves every
//! entry point; the HTTP and WebSocket person paths enqueue here, and a
//! channel's or a schedule's brief joins the same queue through the same
//! function.
//!
//! Placed here because the queue is a session-level fact between turns: the
//! coordinator owns a turn; this module owns what waits for the next one.

use std::sync::Arc;

use tokio::sync::{Mutex, mpsc};
use uuid::Uuid;

use swage_core::GatewayError;
use swage_protocol::client::conversation::QueuedMessageView;
use swage_protocol::client::websocket::ServerEvent;
use swage_storage::TurnState;
use swage_storage::model::{MessageOrigin, NewQueuedTurnInput, QueuedTurnInput};

use crate::service::{announce_appended_message, message_view, pending_turn_state};
use crate::state::AppState;
use crate::subscription::SubscriptionState;
use crate::turn::{TurnAttendance, TurnInput, attachment_records, message_attachments};

/// The wire view of a queued message.
pub fn queued_message_view(queued: &QueuedTurnInput) -> Result<QueuedMessageView, GatewayError> {
    Ok(QueuedMessageView {
        id: queued.id,
        origin: queued.origin,
        content: queued.content.clone(),
        attachments: message_attachments(&queued.attachments)?,
        queued_at: queued.created_at,
    })
}

/// Accept a message that arrived while the session's turn is running: hold it
/// durably and tell every open view it is queued. Returns the queued view;
/// the caller answers its client with it.
pub async fn queue_input(
    state: &AppState,
    session_id: Uuid,
    thread_id: Uuid,
    input: TurnInput,
) -> Result<QueuedMessageView, GatewayError> {
    let queued = state
        .storage
        .enqueue_turn_input(NewQueuedTurnInput {
            session_id,
            thread_id,
            origin: input.origin,
            content: input.text,
            attachments: attachment_records(&input.attachments),
        })
        .await?;
    let view = queued_message_view(&queued)?;
    let _ = state.broadcast.send(ServerEvent::MessageQueued {
        session_id,
        queued: view.clone(),
    });
    // The running turn may have ended between the caller's check and this
    // enqueue, leaving nobody to drain; a drain that finds a turn running
    // simply leaves the queue to that turn's end.
    if !state.active_turns.lock().await.contains_key(&session_id) {
        spawn_drain(state.clone(), session_id);
    }
    Ok(view)
}

/// Whether a turn's ending hands the session to its queue. A completed or
/// failed turn does; a cancelled one does not — stopping a turn leaves the
/// queue standing for the person to look at.
pub fn drains_queue_after(turn_state: TurnState) -> bool {
    !matches!(turn_state, TurnState::Interrupted)
}

/// Run the queue's drain on its own task, so the ending turn's own path is
/// not held open by the next turn.
pub fn spawn_drain(state: AppState, session_id: Uuid) {
    tokio::spawn(async move {
        match drain_queue(&state, session_id).await {
            Ok(Some(turn_id)) => {
                tracing::info!(%session_id, %turn_id, "queued messages ran as the next turn")
            }
            Ok(None) => {}
            Err(error) => tracing::warn!(%session_id, %error, "turn queue drain failed"),
        }
    });
}

/// Run everything queued for the session as one turn: the store claims the
/// session with the turn, records each queued message as its own row with
/// its origin, and empties the queue in one transaction, so a claim that
/// loses to a live turn changes nothing and a failure leaves no half-state.
/// `None` when nothing was queued or a turn is already running (the queue
/// waits for that turn's end).
pub async fn drain_queue(state: &AppState, session_id: Uuid) -> Result<Option<Uuid>, GatewayError> {
    if state.active_turns.lock().await.contains_key(&session_id) {
        return Ok(None);
    }
    let session = state
        .storage
        .get_session(session_id)
        .await?
        .ok_or(GatewayError::ConversationNotFound { id: session_id })?;
    let Some(drained) = state
        .storage
        .drain_queued_turn_inputs(session_id, &session.agent_id)
        .await?
    else {
        return Ok(None);
    };
    let turn = drained.turn;
    let thread_id = turn.thread_id;

    // Every recorded row reaches every open view; then the queue is
    // announced empty.
    for recorded in &drained.messages {
        announce_appended_message(
            state,
            session_id,
            message_view(recorded.clone(), pending_turn_state(), Vec::new())?,
        );
    }
    let _ = state.broadcast.send(ServerEvent::QueueDrained {
        session_id,
        turn_id: turn.id,
        queued_ids: drained.queued_ids.clone(),
    });

    // The turn's events reach every open view of the session through the
    // per-session channel: no single client owns a turn the queue started.
    let (events_tx, mut events_rx) = mpsc::unbounded_channel::<ServerEvent>();
    let listeners = state.clone();
    tokio::spawn(async move {
        while let Some(event) = events_rx.recv().await {
            listeners.send_to_session(session_id, &event).await;
        }
    });
    let input = TurnInput {
        text: turn.user_input.clone().unwrap_or_default(),
        // Attachments already sit on the recorded rows; the turn rebuilds
        // its history from them.
        attachments: Vec::new(),
        origin: drained
            .messages
            .first()
            .map(|message| message.origin)
            .unwrap_or(MessageOrigin::User),
    };
    Box::pin(crate::turn::process_turn_with_composition_and_model(
        state,
        session_id,
        Some(thread_id),
        input,
        events_tx,
        Arc::new(Mutex::new(SubscriptionState::new(
            state.active_config().observability.force_on,
        ))),
        None,
        None,
        None,
        None,
        Some(turn.id),
        TurnAttendance::Attended,
        Default::default(),
        None,
        None,
        None,
        true,
    ))
    .await?;
    Ok(Some(turn.id))
}

/// After a restart: every session with a queue and no live turn runs its
/// queue, once the orphaned turns have been settled.
pub async fn drain_queues_at_startup(state: &AppState) -> Result<usize, GatewayError> {
    let sessions = state.storage.sessions_with_queued_turn_inputs().await?;
    let count = sessions.len();
    for session_id in sessions {
        spawn_drain(state.clone(), session_id);
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use swage_core::{FinishReason, Role, StreamChunk};
    use swage_storage::MessageOrigin;
    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::thread_selection::resolve_thread_for_session;
    use crate::turn::tests::make_state_with_session;

    fn reply(text: &str) -> Vec<StreamChunk> {
        vec![
            StreamChunk::Delta {
                content: text.into(),
            },
            StreamChunk::Done {
                finish_reason: Some(FinishReason::Stop),
            },
        ]
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn messages_arriving_mid_turn_queue_in_order_and_run_as_the_next_turn() {
        let (state, session_id) = make_state_with_session(reply("Both answered.")).await;
        let thread = resolve_thread_for_session(state.storage.as_ref(), session_id, None)
            .await
            .unwrap();
        let mut announcements = state.broadcast.subscribe();
        let (listener, mut session_events) = mpsc::unbounded_channel::<ServerEvent>();
        state.listen_to_session(session_id, listener).await;

        // A turn is running: the session holds its guard.
        state
            .active_turns
            .lock()
            .await
            .insert(session_id, CancellationToken::new());

        let first = queue_input(
            &state,
            session_id,
            thread.id,
            TurnInput::text("first thought"),
        )
        .await
        .expect("queued");
        let second = queue_input(
            &state,
            session_id,
            thread.id,
            TurnInput::text("and another").with_origin(MessageOrigin::Schedule),
        )
        .await
        .expect("queued");
        assert_eq!(first.origin, MessageOrigin::User);
        assert_eq!(second.origin, MessageOrigin::Schedule);

        // Both are visible as queued, in arrival order, and were announced.
        let listed = state
            .storage
            .list_queued_turn_inputs(session_id)
            .await
            .unwrap();
        assert_eq!(
            listed
                .iter()
                .map(|q| q.content.as_str())
                .collect::<Vec<_>>(),
            vec!["first thought", "and another"]
        );
        let queued_events = std::iter::from_fn(|| announcements.try_recv().ok())
            .filter(|event| matches!(event, ServerEvent::MessageQueued { .. }))
            .count();
        assert_eq!(queued_events, 2);
        // Nothing ran: the guard is still held.
        assert!(
            state
                .storage
                .list_messages_by_thread(thread.id)
                .await
                .unwrap()
                .is_empty()
        );

        // The running turn ends: the queue runs as one turn.
        state.active_turns.lock().await.remove(&session_id);
        let turn_id = drain_queue(&state, session_id)
            .await
            .expect("drain")
            .expect("a turn ran");

        let messages = state
            .storage
            .list_messages_by_thread(thread.id)
            .await
            .unwrap();
        let recorded: Vec<(&str, MessageOrigin, Role)> = messages
            .iter()
            .map(|m| (m.content.as_str(), m.origin, m.role))
            .collect();
        assert_eq!(
            recorded,
            vec![
                ("first thought", MessageOrigin::User, Role::User),
                ("and another", MessageOrigin::Schedule, Role::User),
                ("Both answered.", MessageOrigin::Assistant, Role::Assistant),
            ],
            "each queued message is its own row with its origin, then the reply"
        );
        assert!(messages[..2].iter().all(|m| m.turn_id == Some(turn_id)));
        let turn = state.storage.get_turn(turn_id).await.unwrap().unwrap();
        assert_eq!(turn.state, TurnState::Completed);
        assert_eq!(
            turn.user_input.as_deref(),
            Some("first thought\n\nand another")
        );
        assert!(
            state
                .storage
                .list_queued_turn_inputs(session_id)
                .await
                .unwrap()
                .is_empty(),
            "the queue is empty once its turn has started"
        );

        // Open views learned of each recorded row and of the queue emptying,
        // and the turn's own events reached the session channel.
        let later: Vec<ServerEvent> =
            std::iter::from_fn(|| announcements.try_recv().ok()).collect();
        assert_eq!(
            later
                .iter()
                .filter(|e| matches!(e, ServerEvent::MessageAppended { session_id: s, .. } if *s == session_id))
                .count(),
            2
        );
        assert!(later.iter().any(|e| matches!(e, ServerEvent::QueueDrained { session_id: s, turn_id: t, .. } if *s == session_id && *t == turn_id)));
        let session_event_kinds: Vec<&'static str> =
            std::iter::from_fn(|| session_events.try_recv().ok())
                .map(|event| match event {
                    ServerEvent::Token { .. } => "token",
                    ServerEvent::TurnComplete { .. } => "turn_complete",
                    _ => "other",
                })
                .collect();
        assert!(session_event_kinds.contains(&"token"));
        assert!(session_event_kinds.contains(&"turn_complete"));

        // Nothing left to drain.
        assert!(drain_queue(&state, session_id).await.unwrap().is_none());
    }

    /// A turn that fails before its agent runs ends through the same release
    /// as a completed one, so the queue behind it runs rather than waiting for
    /// an unrelated message.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_turn_that_fails_before_running_still_hands_the_session_to_its_queue() {
        use swage_storage::NewTurn;

        let (state, session_id) = make_state_with_session(reply("Ran after the failure.")).await;
        let thread = resolve_thread_for_session(state.storage.as_ref(), session_id, None)
            .await
            .unwrap();
        let failing = state
            .storage
            .create_turn(NewTurn {
                thread_id: thread.id,
                agent_id: "gateway".into(),
                state: TurnState::Pending,
                user_input: Some("doomed".into()),
                parent_turn_id: None,
            })
            .await
            .unwrap();
        state
            .active_turns
            .lock()
            .await
            .insert(session_id, CancellationToken::new());
        queue_input(&state, session_id, thread.id, TurnInput::text("after you"))
            .await
            .unwrap();

        crate::turn::turn_release::record_terminal_state_and_release(
            &state,
            session_id,
            failing.id,
            TurnState::Failed,
        )
        .await
        .unwrap();

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let turns = state.storage.list_turns_by_thread(thread.id).await.unwrap();
            if turns.iter().any(|turn| {
                turn.user_input.as_deref() == Some("after you")
                    && turn.state == TurnState::Completed
            }) {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the queued message should have run as the next turn: {turns:?}"
            );
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(
            state
                .storage
                .list_queued_turn_inputs(session_id)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_queue_waits_while_a_turn_still_runs_and_a_stop_does_not_drain_it() {
        let (state, session_id) = make_state_with_session(Vec::new()).await;
        let thread = resolve_thread_for_session(state.storage.as_ref(), session_id, None)
            .await
            .unwrap();
        state
            .active_turns
            .lock()
            .await
            .insert(session_id, CancellationToken::new());
        queue_input(&state, session_id, thread.id, TurnInput::text("later"))
            .await
            .unwrap();

        assert!(
            drain_queue(&state, session_id).await.unwrap().is_none(),
            "a drain under a live turn leaves the queue to that turn's end"
        );
        assert_eq!(
            state
                .storage
                .list_queued_turn_inputs(session_id)
                .await
                .unwrap()
                .len(),
            1
        );
        // A stopped turn ends Interrupted: the queue stands.
        assert!(!drains_queue_after(TurnState::Interrupted));
        assert!(drains_queue_after(TurnState::Completed));
        assert!(drains_queue_after(TurnState::Failed));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_restart_drains_every_queue_it_finds() {
        let (state, session_id) = make_state_with_session(reply("Caught up.")).await;
        let thread = resolve_thread_for_session(state.storage.as_ref(), session_id, None)
            .await
            .unwrap();
        state
            .storage
            .enqueue_turn_input(NewQueuedTurnInput {
                session_id,
                thread_id: thread.id,
                origin: MessageOrigin::User,
                content: "sent before the restart".into(),
                attachments: Vec::new(),
            })
            .await
            .unwrap();

        let drained = drain_queues_at_startup(&state).await.unwrap();
        assert_eq!(drained, 1);
        // The drain runs on its own task: wait for the turn it started.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let messages = state
                .storage
                .list_messages_by_thread(thread.id)
                .await
                .unwrap();
            if messages.iter().any(|m| m.content == "Caught up.") {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the queued turn did not run"
            );
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    }
}

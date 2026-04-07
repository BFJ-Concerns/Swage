use std::collections::HashSet;

use uuid::Uuid;

use swage_core::GatewayError;
use swage_storage::{Thread, store::Storage};

/// Resolve the thread a session-scoped operation should target.
///
/// When `requested_thread_id` is present, it must belong to the session.
/// Otherwise the session's single root thread is resolved explicitly rather
/// than relying on creation-order accidents.
pub(crate) async fn resolve_thread_for_session(
    storage: &dyn Storage,
    session_id: Uuid,
    requested_thread_id: Option<Uuid>,
) -> Result<Thread, GatewayError> {
    let threads = storage.list_by_session(session_id).await?;
    if threads.is_empty() {
        return Err(GatewayError::ConversationNotFound { id: session_id });
    }

    if let Some(thread_id) = requested_thread_id {
        return threads
            .into_iter()
            .find(|thread| thread.id == thread_id)
            .ok_or_else(|| {
                GatewayError::ValidationError(format!(
                    "thread {thread_id} does not belong to conversation {session_id}"
                ))
            });
    }

    let child_thread_ids: HashSet<Uuid> = storage
        .list_branches_by_session(session_id)
        .await?
        .into_iter()
        .map(|branch| branch.child_thread_id)
        .collect();

    let mut root_threads = threads
        .into_iter()
        .filter(|thread| !child_thread_ids.contains(&thread.id));

    let root_thread = root_threads.next().ok_or_else(|| {
        GatewayError::ValidationError(format!("conversation {session_id} has no root thread"))
    })?;

    if root_threads.next().is_some() {
        return Err(GatewayError::ValidationError(format!(
            "conversation {session_id} has multiple root threads"
        )));
    }

    Ok(root_thread)
}

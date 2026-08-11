//! What the audit trail promises about a turn, and the escalation path
//! when part of that promise cannot be kept.
//!
//! ## The reconstructability contract
//!
//! Everything the platform does is reconstructable afterwards from the one
//! local record — `swage.db` plus the content-addressed body files. That
//! promise is not uniform: a turn's records fall into three classes, and
//! each class has one prescribed response to a persistence failure. A
//! persistence seam in the turn path must name its class; a bare
//! warn-and-continue is a contract violation.
//!
//! 1. **Constitutive** — the record *is* the work: turn rows and state
//!    transitions, thread messages, tool calls, approvals, configuration
//!    changes, structured output. A turn whose constitutive record is lost
//!    did not auditably happen, so the loss fails the turn loudly — the
//!    error propagates (or, past the point of no return, the turn is
//!    marked `Failed` and the client told why).
//! 2. **Reconstructive** — enrichments that explain the work without being
//!    it: context snapshots, provider-visible request captures, reasoning
//!    captures. Their loss degrades the audit trail but does not falsify
//!    it, so the response is an explicit degradation record: a
//!    `turn_record_degraded` audit event naming what is missing, written
//!    by [`record_reconstruction_gap`]. The trail then states what it
//!    cannot reconstruct instead of silently lacking it. If even that
//!    event cannot be written, the store is not keeping any promise — the
//!    failure escalates to class 1 and the turn fails loudly.
//! 3. **Derivable** — metadata reconstructable from constitutive rows,
//!    such as session activity timestamps (derivable from turn and
//!    message times). Proven safe to lose; warn-and-continue is the
//!    accepted disposition, stated at the site with a reference here.

use std::fmt::Display;

use uuid::Uuid;

use swage_core::GatewayError;
use swage_storage::{NewAuditEvent, Storage};

/// Record that a reconstructive turn record could not be persisted.
///
/// The audit event is the degradation record the contract's class 2
/// requires: `record` names the missing piece in the trail's own terms
/// (`context_snapshot`, `provider_visible_snapshot`, …) and the error says
/// why. An `Err` from here means the degradation itself could not be
/// recorded — the caller must escalate to failing the turn, not log and
/// move on. A *constitutive* (class 1) seam may also call this so the
/// trail names its gap, but calling it is never that seam's whole
/// response: class 1 loss fails the turn regardless of whether this event
/// lands (the structured-output seam in turn.rs is the worked example).
pub(crate) async fn record_reconstruction_gap(
    storage: &dyn Storage,
    session_id: Option<Uuid>,
    turn_id: Uuid,
    record: &'static str,
    error: &(dyn Display + Send + Sync),
) -> Result<(), GatewayError> {
    storage
        .create_audit_event(NewAuditEvent {
            id: format!("turn-record-degraded-{}", Uuid::new_v4()),
            session_id,
            entity_kind: "turn".into(),
            entity_id: turn_id.to_string(),
            event_kind: "turn_record_degraded".into(),
            summary: format!("A turn record could not be persisted: {record}"),
            metadata: serde_json::json!({
                "record": record,
                "error": error.to_string(),
            }),
        })
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use swage_storage::{AuditStore, SqliteStorage};

    #[tokio::test(flavor = "multi_thread")]
    async fn reconstruction_gap_lands_in_the_audit_trail() {
        let storage = SqliteStorage::in_memory().unwrap();
        let turn_id = Uuid::new_v4();

        record_reconstruction_gap(&storage, None, turn_id, "context_snapshot", &"disk full")
            .await
            .unwrap();

        let events = storage.list_audit_events().await.unwrap();
        let event = events
            .iter()
            .find(|e| e.event_kind == "turn_record_degraded")
            .expect("the degradation is itself a record");
        assert_eq!(event.entity_id, turn_id.to_string());
        assert_eq!(event.metadata["record"], "context_snapshot");
        assert_eq!(event.metadata["error"], "disk full");
    }
}

//! Configuration changes are ordinary granted operations, recorded.
//!
//! There is no separate authoring flow: a mutation surface (an HTTP
//! handler, a gateway tool, an import) validates its input, applies it
//! through the resource's own store, and records one `configuration_change`
//! audit event naming what changed and what it replaced. Recovery is making
//! another change, informed by that record — the audit trail is the
//! history; there is no version store and no undo machinery.
//!
//! The record carries the whole prior and new state of the named entity,
//! not a derived diff: a reader maps the event to the setting that changed
//! by comparing the two states under a named action.
//!
//! The apply and its record are separate storage writes, so a crash or
//! storage failure between them can leave an applied change without its
//! record. That window is not silently accepted: a recording failure turns
//! the whole operation into an error naming the applied-but-unrecorded
//! state, so no caller ever reports a recorded change that is not one.

use uuid::Uuid;

use swage_core::GatewayError;

use crate::state::AppState;

/// One configuration change, ready to record.
///
/// `entity_kind`/`entity_id` name the resource at registry grain;
/// `action` is the operation performed (`create_schedule`,
/// `upsert_agent_definition`, …); `source` names the surface the change
/// came from (a tool name, `operator_http`) and `session_id` ties it to
/// the conversation that authored it, when one did. `prior_state` and
/// `new_state` are whole-entity snapshots — `None` where the entity did
/// not exist on that side of the change.
pub struct ConfigChange<'a> {
    pub entity_kind: &'a str,
    pub entity_id: &'a str,
    pub action: &'a str,
    pub source: &'a str,
    pub session_id: Option<Uuid>,
    pub summary: &'a str,
    pub prior_state: Option<serde_json::Value>,
    pub new_state: Option<serde_json::Value>,
}

/// Record an applied configuration change to the audit trail.
///
/// Call this after the store write succeeds, with the snapshots read
/// around it. The event is part of the change, not a courtesy: a change
/// that cannot be recorded is surfaced as the error it is, and the caller
/// must propagate it rather than report success.
pub async fn record_config_change(
    state: &AppState,
    change: ConfigChange<'_>,
) -> Result<(), GatewayError> {
    let summary = change.summary.to_owned();
    state
        .storage
        .create_audit_event(swage_storage::NewAuditEvent {
            id: Uuid::new_v4().to_string(),
            session_id: change.session_id,
            entity_kind: change.entity_kind.to_owned(),
            entity_id: change.entity_id.to_owned(),
            event_kind: "configuration_change".to_owned(),
            summary: summary.clone(),
            metadata: serde_json::json!({
                "action": change.action,
                "source": change.source,
                "prior_state": change.prior_state,
                "new_state": change.new_state,
            }),
        })
        .await
        .map(|_| ())
        .map_err(|error| {
            GatewayError::ConfigError(format!(
                "'{summary}' was applied but recording it failed ({error}); \
                 the configuration is changed without a record — reconcile before further changes"
            ))
        })
}

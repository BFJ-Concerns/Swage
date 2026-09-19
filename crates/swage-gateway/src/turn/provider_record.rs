//! What the provider was shown and what came back, as the turn's durable
//! record: the provider-visible capture in the context snapshot (requests
//! redacted of every registered secret value, response witnesses, reasoning,
//! the effective model and resolved effort) and the per-round provider
//! records opened when a request leaves and settled when its round ends.
//!
//! One seam serves both turn paths — the ordinary coordinator and the
//! delegated child — so redaction cannot drift between them. The durability
//! contract (record_durability.rs) classifies every persistence site here:
//! the provider round record is reconstructive, its loss named through
//! `record_reconstruction_gap` and escalated only when the gap itself cannot
//! be recorded.

use std::mem;

use super::{
    AgentEvent, AppState, GatewayError, Uuid, WorkflowProviderRoundOutcome, record_durability,
};

pub(crate) fn append_provider_visible_request(
    report_json: &mut serde_json::Value,
    round: usize,
    request: serde_json::Value,
) {
    provider_visible_array(report_json, "requests").push(serde_json::json!({
        "round": round,
        "request": request,
    }));
}

pub(crate) fn append_provider_visible_capture_error(
    report_json: &mut serde_json::Value,
    round: usize,
) {
    provider_visible_array(report_json, "capture_errors").push(serde_json::json!({
        "round": round,
        "kind": "secret_redaction_unavailable",
    }));
}

pub(crate) fn append_provider_visible_response(
    report_json: &mut serde_json::Value,
    round: usize,
    witness: swage_core::ProviderResponseWitness,
) {
    provider_visible_array(report_json, "responses").push(serde_json::json!({
        "round": round,
        "witness": witness,
    }));
}

pub(crate) async fn redact_provider_visible_request(
    state: &AppState,
    mut request: serde_json::Value,
) -> Result<serde_json::Value, swage_core::StorageError> {
    let secret_values = known_secret_values(state).await?;
    if !secret_values.is_empty() {
        redact_secret_values(&mut request, &secret_values);
    }
    Ok(request)
}

/// Apply a provider diagnostic event to its durable context report. Both the
/// ordinary and delegated turn paths use this seam so request redaction cannot
/// drift from response and reasoning capture.
#[derive(Debug, Clone, Copy)]
pub(crate) enum ProviderSnapshotCapturePath {
    Ordinary,
    Delegated,
}

pub(crate) async fn apply_provider_snapshot_event(
    state: &AppState,
    report_json: &mut serde_json::Value,
    event: AgentEvent,
    capture_path: ProviderSnapshotCapturePath,
) {
    let turn_id = match &event {
        AgentEvent::ProviderRequestPrepared { turn_id, .. }
        | AgentEvent::ProviderResponseCaptured { turn_id, .. }
        | AgentEvent::ProviderReasoningCaptured { turn_id, .. } => *turn_id,
        _ => return,
    };
    match event {
        AgentEvent::ProviderRequestPrepared { round, request, .. } => {
            match redact_provider_visible_request(state, request).await {
                Ok(request) => append_provider_visible_request(report_json, round, request),
                Err(error) => {
                    match capture_path {
                        ProviderSnapshotCapturePath::Ordinary => tracing::error!(
                            turn_id = %turn_id,
                            round,
                            error = %error,
                            "provider-visible request capture omitted because secret redaction was unavailable"
                        ),
                        ProviderSnapshotCapturePath::Delegated => tracing::error!(
                            turn_id = %turn_id,
                            round,
                            error = %error,
                            "delegated provider-visible request capture omitted because secret redaction was unavailable"
                        ),
                    }
                    append_provider_visible_capture_error(report_json, round);
                }
            }
        }
        AgentEvent::ProviderResponseCaptured { round, witness, .. } => {
            append_provider_visible_response(report_json, round, witness);
        }
        AgentEvent::ProviderReasoningCaptured {
            round,
            content,
            details,
            ..
        } => {
            append_provider_visible_reasoning(report_json, round, content, details);
        }
        _ => unreachable!("non-provider event returned above"),
    }
}

async fn known_secret_values(state: &AppState) -> Result<Vec<String>, swage_core::StorageError> {
    state
        .secret_manager
        .known_values()
        .await
        .map_err(|error| match error {
            GatewayError::Storage(storage) => storage,
            other => swage_core::StorageError::Corruption {
                message: format!("secret redaction set could not be read: {other}"),
            },
        })
}

fn redact_secret_values(value: &mut serde_json::Value, secret_values: &[String]) {
    match value {
        serde_json::Value::String(text) => {
            let mut redacted = mem::take(text);
            for secret in secret_values {
                redacted = redacted.replace(secret, "[REDACTED_SECRET]");
            }
            *text = redacted;
        }
        serde_json::Value::Array(values) => {
            for value in values {
                redact_secret_values(value, secret_values);
            }
        }
        serde_json::Value::Object(values) => {
            for value in values.values_mut() {
                redact_secret_values(value, secret_values);
            }
        }
        serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::Number(_) => {}
    }
}

pub(crate) fn append_provider_visible_reasoning(
    report_json: &mut serde_json::Value,
    round: usize,
    content: String,
    details: Option<serde_json::Value>,
) {
    provider_visible_array(report_json, "reasoning").push(serde_json::json!({
        "round": round,
        "content": content,
        "details": details,
    }));
}

fn normalise_report_object(report_json: &mut serde_json::Value) {
    if !report_json.is_object() {
        let previous = std::mem::replace(report_json, serde_json::json!({}));
        report_json["assembly_report"] = previous;
    }
}

fn provider_visible_array<'a>(
    report_json: &'a mut serde_json::Value,
    key: &str,
) -> &'a mut Vec<serde_json::Value> {
    normalise_report_object(report_json);

    let object = report_json
        .as_object_mut()
        .expect("report_json was normalised to an object");
    let provider_visible = object
        .entry("provider_visible".to_string())
        .or_insert_with(|| serde_json::json!({}));
    if !provider_visible.is_object() {
        *provider_visible = serde_json::json!({});
    }

    let provider_visible = provider_visible
        .as_object_mut()
        .expect("provider_visible was normalised to an object");
    let values = provider_visible
        .entry(key.to_string())
        .or_insert_with(|| serde_json::json!([]));
    if !values.is_array() {
        *values = serde_json::json!([]);
    }
    values
        .as_array_mut()
        .expect("provider-visible value was normalised to an array")
}

/// Open the record for a provider request that has just gone out.
///
/// The row's start time with no settlement is what tells an outstanding
/// provider request from a tool loop that is making progress, so it is
/// written when the request leaves rather than when the round ends.
/// Reconstructive record (record_durability.rs): losing it degrades the
/// trail's account of the round without falsifying the turn, so the gap is
/// named rather than swallowed, and a gap that cannot itself be recorded
/// escalates.
pub(super) async fn open_provider_round_record(
    state: &AppState,
    session_id: Uuid,
    turn_id: Uuid,
    round: usize,
    record_integrity_lost: &mut Option<String>,
) {
    let outcome = match serde_json::to_value(WorkflowProviderRoundOutcome::Outstanding) {
        Ok(outcome) => outcome,
        Err(error) => {
            record_integrity_lost.get_or_insert(format!(
                "provider round outcome could not be serialised: {error}"
            ));
            return;
        }
    };
    if let Err(error) = state
        .storage
        .open_provider_round(turn_id, round as i64, outcome)
        .await
    {
        tracing::warn!(error = %error, round, "failed to record provider round start");
        if let Err(audit_error) = record_durability::record_reconstruction_gap(
            state.storage.as_ref(),
            Some(session_id),
            turn_id,
            "provider_round",
            &error,
        )
        .await
        {
            record_integrity_lost.get_or_insert(format!(
                "provider round record lost and the loss could not be recorded: {audit_error}"
            ));
        }
    }
}

/// Settle the record for a provider round that has ended, with what came
/// back. Same reconstructive class and escalation as the opening record.
pub(super) async fn settle_provider_round_record(
    state: &AppState,
    session_id: Uuid,
    turn_id: Uuid,
    round: usize,
    outcome: WorkflowProviderRoundOutcome,
    record_integrity_lost: &mut Option<String>,
) {
    let outcome = match serde_json::to_value(outcome) {
        Ok(outcome) => outcome,
        Err(error) => {
            record_integrity_lost.get_or_insert(format!(
                "provider round outcome could not be serialised: {error}"
            ));
            return;
        }
    };
    if let Err(error) = state
        .storage
        .settle_provider_round(turn_id, round as i64, outcome)
        .await
    {
        tracing::warn!(error = %error, round, "failed to record provider round outcome");
        if let Err(audit_error) = record_durability::record_reconstruction_gap(
            state.storage.as_ref(),
            Some(session_id),
            turn_id,
            "provider_round",
            &error,
        )
        .await
        {
            record_integrity_lost.get_or_insert(format!(
                "provider round record lost and the loss could not be recorded: {audit_error}"
            ));
        }
    }
}

/// Record the resolved effort beside the effective model in the turn's
/// provider-visible snapshot: what was asked, what is sent, and any clamp.
pub(super) fn record_effort_snapshot(
    report_json: &mut serde_json::Value,
    resolved: &swage_core::ResolvedEffort,
) {
    normalise_report_object(report_json);
    if !report_json["provider_visible"].is_object() {
        report_json["provider_visible"] = serde_json::json!({});
    }
    report_json["provider_visible"]["effort"] =
        serde_json::to_value(resolved).expect("resolved effort serialises");
}

pub(super) fn record_effective_model_snapshot(
    report_json: &mut serde_json::Value,
    catalogue_entry: &str,
    model: &swage_config::ModelEndpointConfig,
    source: &str,
) {
    normalise_report_object(report_json);

    if !report_json["provider_visible"].is_object() {
        report_json["provider_visible"] = serde_json::json!({});
    }
    report_json["provider_visible"]["effective_model"] = serde_json::json!({
        "catalogue_entry": catalogue_entry,
        "model_id": model.model_id,
        "provider_id": model.provider_id,
        "source": source,
    });
}

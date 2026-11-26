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
        | AgentEvent::ProviderReasoningCaptured { turn_id, .. }
        | AgentEvent::ProviderAttempt { turn_id, .. } => *turn_id,
        _ => return,
    };
    match event {
        AgentEvent::ProviderAttempt { mut record, .. } => {
            if let Some(request) = record.request.take() {
                record.request = match redact_provider_visible_request(state, request).await {
                    Ok(request) => Some(request),
                    Err(_) => {
                        append_provider_visible_capture_error(report_json, record.round);
                        None
                    }
                };
                let requests = provider_visible_array(report_json, "requests");
                // Replace the provisional pre-stream snapshot with concrete
                // attempts. Settlements update the same entry, preserving the
                // order in which attempts ran, even if capture failed.
                requests.retain(|entry| {
                    entry["round"] != record.round || !entry["attempt_id"].is_null()
                });
                let value = serde_json::json!({
                    "round": record.round,
                    "attempt_id": record.id,
                    "request": record.request,
                });
                if let Some(previous) = requests
                    .iter_mut()
                    .find(|entry| entry["attempt_id"] == record.id)
                {
                    *previous = value;
                } else {
                    requests.push(value);
                }
            }
            if let Some(error) = &mut record.error {
                match known_secret_values(state).await {
                    Ok(secrets) => {
                        redact_text(error, &secrets);
                    }
                    Err(_) => {
                        *error =
                            "Error detail omitted because secret redaction was unavailable".into();
                    }
                }
            }
            let value = serde_json::to_value(&record).expect("attempt serialises");
            let attempts = report_json
                .as_object_mut()
                .expect("context report is an object")
                .entry("provider_attempts")
                .or_insert_with(|| serde_json::json!([]))
                .as_array_mut()
                .expect("provider attempts is an array");
            if let Some(previous) = attempts.iter_mut().find(|a| a["id"] == record.id) {
                *previous = value;
            } else {
                attempts.push(value);
            }
        }
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

pub(super) async fn known_secret_values(
    state: &AppState,
) -> Result<Vec<String>, swage_core::StorageError> {
    registered_secret_values(&state.secret_manager).await
}

/// Every registered secret's value, longest first so that a value containing
/// another is masked whole.
pub(crate) async fn registered_secret_values(
    secret_manager: &crate::secret::SecretManager,
) -> Result<Vec<String>, swage_core::StorageError> {
    let mut values = secret_manager
        .known_values()
        .await
        .map_err(|error| match error {
            GatewayError::Storage(storage) => storage,
            other => swage_core::StorageError::Corruption {
                message: format!("secret redaction set could not be read: {other}"),
            },
        })?;
    values.sort_unstable_by_key(|value| std::cmp::Reverse(value.len()));
    Ok(values)
}

pub(super) const REDACTED_SECRET: &str = "[REDACTED_SECRET]";

pub(super) fn redact_text(text: &mut String, secret_values: &[impl AsRef<str>]) -> bool {
    let mut replaced = false;
    for secret in secret_values {
        let secret = secret.as_ref();
        if text.contains(secret) {
            *text = text.replace(secret, REDACTED_SECRET);
            replaced = true;
        }
    }
    replaced
}

/// Mask every registered value in text that may hold them inside serialised
/// JSON, where a value with quotes, backslashes or control characters appears
/// escaped: both the escaped and the plain form are replaced.
pub(crate) fn redact_serialised_text(text: &mut String, secret_values: &[String]) -> bool {
    let mut changed = false;
    for secret in secret_values {
        let encoded = serde_json::to_string(secret).expect("secret text serialises");
        let escaped = &encoded[1..encoded.len() - 1];
        if escaped != secret.as_str() {
            changed |= redact_text(text, &[escaped]);
        }
        changed |= redact_text(text, &[secret.as_str()]);
    }
    changed
}

pub(super) fn redact_secret_values(
    value: &mut serde_json::Value,
    secret_values: &[String],
) -> bool {
    let mut replaced = false;
    match value {
        serde_json::Value::String(text) => return redact_text(text, secret_values),
        serde_json::Value::Array(values) => {
            for value in values {
                replaced |= redact_secret_values(value, secret_values);
            }
        }
        serde_json::Value::Object(values) => {
            for value in values.values_mut() {
                replaced |= redact_secret_values(value, secret_values);
            }
        }
        serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::Number(_) => {}
    }
    replaced
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

/// Record the output limit the turn's request carries beside the effort:
/// the tokens sent and which source named them. Absent from the snapshot
/// when no source declared one — the adapter's protocol floor then applies,
/// and the per-round request record shows what went on the wire.
pub(super) fn record_output_limit_snapshot(
    report_json: &mut serde_json::Value,
    resolved: swage_core::ResolvedOutputLimit,
) {
    normalise_report_object(report_json);
    if !report_json["provider_visible"].is_object() {
        report_json["provider_visible"] = serde_json::json!({});
    }
    report_json["provider_visible"]["output_limit"] =
        serde_json::to_value(resolved).expect("resolved output limit serialises");
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

pub(crate) async fn redact_element_texts(
    state: &AppState,
    report: &mut swage_protocol::client::context::AssemblyReport,
) {
    fn visit(
        elements: &mut [swage_protocol::client::context::ElementReport],
        secrets: Option<&[String]>,
    ) {
        for element in elements {
            if let Some(secrets) = secrets {
                if let Some(text) = &mut element.full_text {
                    redact_text(text, secrets);
                }
            } else {
                element.full_text = None;
            }
            if let Some(nested) = &mut element.named_assembly {
                visit(&mut nested.child_elements, secrets);
            }
        }
    }
    match known_secret_values(state).await {
        Ok(secrets) => visit(&mut report.elements, Some(&secrets)),
        Err(_) => {
            visit(&mut report.elements, None);
            report.warnings.push(
                "Element text capture omitted because secret redaction was unavailable".into(),
            );
        }
    }
}

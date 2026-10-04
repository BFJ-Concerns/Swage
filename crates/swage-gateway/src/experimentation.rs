//! Variant selection — the runtime half of the experimentation seam.
//!
//! Where a behaviour could be done more than one way, the alternatives are
//! registered as variants behind a stable surface and one is selected per
//! use. Selection honours the configuration seam that already exists
//! (`ExperimentationConfig`: the global kill switch, per-location
//! enablement, forced variants) and records every decision to the audit
//! trail, so any run can be reconstructed — which variant served it and
//! why.
//!
//! Blinding is the contract: the variant id appears in the audit record and
//! nowhere else. No user-facing or model-facing surface may branch on it or
//! display it; forcing a variant through config is the deliberate
//! unblinding step.
//!
//! The first location is context assembly (`context.assembly`). A new
//! location is a location-id constant, a `select_variant` call at the
//! choice point, and the audit write — no new tables or config shapes.

use uuid::Uuid;

use swage_config::model::ExperimentationConfig;

use crate::state::AppState;

/// The context-assembly choice point: which named assembly composes a turn.
pub const LOCATION_CONTEXT_ASSEMBLY: &str = "context.assembly";

/// The working-norms choice point: which norms text a bare-proxy model's
/// session receives for its family. The authored default serves until
/// alternatives register; every draw is recorded like any location's.
pub const LOCATION_WORKING_NORMS: &str = "context.working_norms";
pub const LOCATION_TASK_PROMPT: &str = "task.prompt";

/// The memory curator's prompt: the instructions every curation pass runs
/// under. The authored default is the first variant.
pub const LOCATION_MEMORY_CURATOR_PROMPT: &str = "prompts.memory_curator";

/// Why the selected variant was selected. Recorded verbatim; the reasons are
/// the audit trail's vocabulary, so they name situations, not code paths.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SelectionReason {
    /// Experimentation is off (globally or for this location); the default
    /// serves.
    Disabled,
    /// The draw could not be recorded, so the default serves — an
    /// unrecorded variant never shapes behaviour.
    RecordUnavailable,
    /// The operator pinned this variant for the location.
    Forced,
    /// Drawn from the registered variants.
    Drawn,
    /// Nothing to choose between — no alternative variants are registered.
    DefaultOnly,
}

/// The outcome of one variant selection.
#[derive(Debug, Clone)]
pub struct VariantDecision {
    pub variant: String,
    pub reason: SelectionReason,
}

/// A forced variant names no candidate registered at its choice point.
#[derive(Debug, thiserror::Error)]
#[error(
    "experimentation location '{location_id}' forced unknown variant '{forced_variant}'; available variants: {available_variants:?}"
)]
pub struct VariantSelectionError {
    pub location_id: String,
    pub forced_variant: String,
    pub available_variants: Vec<String>,
}

/// Select the variant that serves one use of a location.
///
/// `registered` lists the alternatives (the default may be among them);
/// `default` is what serves when experimentation cannot or need not choose.
/// The draw is seeded by `draw_seed`; session-scoped callers use the session
/// id, so a recorded decision is reproducible from the audit trail alone.
pub fn select_variant(
    config: &ExperimentationConfig,
    location_id: &str,
    registered: &[&str],
    default: &str,
    draw_seed: Uuid,
) -> Result<VariantDecision, VariantSelectionError> {
    if !config.is_enabled_for(location_id) {
        return Ok(VariantDecision {
            variant: default.to_owned(),
            reason: SelectionReason::Disabled,
        });
    }

    // The candidate pool is the registered variants plus the default.
    let mut pool: Vec<&str> = Vec::with_capacity(registered.len() + 1);
    if !registered.contains(&default) {
        pool.push(default);
    }
    pool.extend_from_slice(registered);

    if let Some(forced) = config.forced_variant_for(location_id) {
        if !pool.contains(&forced) {
            return Err(VariantSelectionError {
                location_id: location_id.to_owned(),
                forced_variant: forced.to_owned(),
                available_variants: pool.iter().map(|variant| (*variant).to_owned()).collect(),
            });
        }
        return Ok(VariantDecision {
            variant: forced.to_owned(),
            reason: SelectionReason::Forced,
        });
    }

    if pool.len() < 2 {
        return Ok(VariantDecision {
            variant: default.to_owned(),
            reason: SelectionReason::DefaultOnly,
        });
    }

    // Uniform draw, seeded from the caller-supplied id: deterministic given
    // the seed, unbiased across seeds. Allocation strategy (bandits) is a
    // deliberate later replacement for this one line.
    let index = (draw_seed.as_u128() % pool.len() as u128) as usize;
    Ok(VariantDecision {
        variant: pool[index].to_owned(),
        reason: SelectionReason::Drawn,
    })
}

/// Serve a session's recorded choice, or draw and record its first use.
/// Configuration changes bind only when a session first uses the location.
/// A failed lookup or write serves the default without an unrecorded draw.
pub async fn select_session_variant(
    state: &AppState,
    config: &ExperimentationConfig,
    location_id: &str,
    registered: &[&str],
    default: &str,
    session_id: Uuid,
    turn_id: Option<Uuid>,
) -> Result<VariantDecision, VariantSelectionError> {
    let fallback = || VariantDecision {
        variant: default.to_owned(),
        reason: SelectionReason::RecordUnavailable,
    };
    let events = match state
        .storage
        .list_audit_events_filtered(swage_storage::AuditEventFilter {
            session_id: Some(session_id),
            entity_kind: Some("experiment_location".to_owned()),
            entity_id: Some(location_id.to_owned()),
            event_kind: Some("variant_selected".to_owned()),
            limit: 1,
            ..Default::default()
        })
        .await
    {
        Ok(events) => events,
        Err(error) => {
            tracing::error!(%error, location_id, "session variant lookup unavailable; serving the default");
            return Ok(fallback());
        }
    };
    if let Some(event) = events.first() {
        match (
            event
                .metadata
                .get("variant")
                .and_then(serde_json::Value::as_str),
            serde_json::from_value::<SelectionReason>(event.metadata["reason"].clone()),
        ) {
            (Some(variant), Ok(reason)) => {
                return Ok(VariantDecision {
                    variant: variant.to_owned(),
                    reason,
                });
            }
            _ => {
                tracing::error!(
                    location_id,
                    "session variant record unreadable; serving the default"
                );
                return Ok(fallback());
            }
        }
    }
    let decision = select_variant(config, location_id, registered, default, session_id)?;
    if let Err(error) =
        record_selection(state, location_id, &decision, Some(session_id), turn_id).await
    {
        tracing::error!(%error, location_id, "session variant record unavailable; serving the default");
        return Ok(fallback());
    }
    Ok(decision)
}

/// Record a selection to the audit trail. Every decision is recorded — the
/// disabled and default-only cases included, because "experimentation did
/// not run here" is itself something a later reader needs to know.
///
/// The record is a precondition of using the variant, not a courtesy after
/// it: an unrecorded draw would shape behaviour invisibly, so on recording
/// failure the caller must not use the drawn variant — fall back to the
/// default (and if even that fallback cannot be recorded, proceed with the
/// default: the default is what serves when experimentation cannot run, and
/// its use is reconstructable from the absence of a record).
pub async fn record_selection(
    state: &AppState,
    location_id: &str,
    decision: &VariantDecision,
    session_id: Option<Uuid>,
    turn_id: Option<Uuid>,
) -> Result<(), swage_core::StorageError> {
    state
        .storage
        .create_audit_event(swage_storage::NewAuditEvent {
            id: Uuid::new_v4().to_string(),
            session_id,
            entity_kind: "experiment_location".to_owned(),
            entity_id: location_id.to_owned(),
            event_kind: "variant_selected".to_owned(),
            summary: format!(
                "location '{location_id}' served by variant '{}'",
                decision.variant
            ),
            metadata: serde_json::json!({
                "variant": decision.variant,
                "reason": decision.reason,
                "turn_id": turn_id,
            }),
        })
        .await
        .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use swage_config::model::ExperimentLocationConfig;

    fn config_with_location(
        enabled: bool,
        location_enabled: bool,
        forced: Option<&str>,
    ) -> ExperimentationConfig {
        let mut config = ExperimentationConfig {
            enabled,
            ..Default::default()
        };
        config.locations.insert(
            LOCATION_CONTEXT_ASSEMBLY.to_owned(),
            ExperimentLocationConfig {
                enabled: location_enabled,
                forced_variant: forced.map(str::to_owned),
            },
        );
        config
    }

    #[test]
    fn kill_switch_always_yields_the_default() {
        let config = config_with_location(false, true, Some("candidate"));
        let decision = select_variant(
            &config,
            LOCATION_CONTEXT_ASSEMBLY,
            &["candidate"],
            "default",
            Uuid::new_v4(),
        )
        .expect("disabled selection should succeed");
        assert_eq!(decision.variant, "default");
        assert_eq!(decision.reason, SelectionReason::Disabled);
    }

    #[test]
    fn per_location_disable_beats_registration() {
        let config = config_with_location(true, false, None);
        let decision = select_variant(
            &config,
            LOCATION_CONTEXT_ASSEMBLY,
            &["candidate"],
            "default",
            Uuid::new_v4(),
        )
        .expect("disabled selection should succeed");
        assert_eq!(decision.reason, SelectionReason::Disabled);
    }

    #[test]
    fn unknown_forced_variant_names_location_rejection_and_available_pool() {
        let config = config_with_location(true, true, Some("typo"));
        let error = select_variant(
            &config,
            LOCATION_CONTEXT_ASSEMBLY,
            &["candidate", "other"],
            "default",
            Uuid::new_v4(),
        )
        .expect_err("unknown force must fail");

        assert_eq!(
            error.to_string(),
            "experimentation location 'context.assembly' forced unknown variant 'typo'; available variants: [\"default\", \"candidate\", \"other\"]"
        );
    }

    #[test]
    fn forced_variant_pins_the_choice() {
        let config = config_with_location(true, true, Some("candidate"));
        let decision = select_variant(
            &config,
            LOCATION_CONTEXT_ASSEMBLY,
            &["candidate", "other"],
            "default",
            Uuid::new_v4(),
        )
        .expect("registered force should succeed");
        assert_eq!(decision.variant, "candidate");
        assert_eq!(decision.reason, SelectionReason::Forced);
    }

    #[test]
    fn no_alternatives_means_default_only_not_a_draw() {
        let config = ExperimentationConfig::default();
        let decision = select_variant(
            &config,
            LOCATION_CONTEXT_ASSEMBLY,
            &[],
            "default",
            Uuid::new_v4(),
        )
        .expect("default-only selection should succeed");
        assert_eq!(decision.variant, "default");
        assert_eq!(decision.reason, SelectionReason::DefaultOnly);
    }

    #[test]
    fn draw_is_reproducible_from_its_seed() {
        let config = ExperimentationConfig::default();
        let seed = Uuid::new_v4();

        let first = select_variant(
            &config,
            LOCATION_CONTEXT_ASSEMBLY,
            &["a", "b", "c"],
            "default",
            seed,
        )
        .expect("draw should succeed");
        let second = select_variant(
            &config,
            LOCATION_CONTEXT_ASSEMBLY,
            &["a", "b", "c"],
            "default",
            seed,
        )
        .expect("draw should succeed");
        assert_eq!(
            first.variant, second.variant,
            "the same seed must reproduce the same decision"
        );
        assert_eq!(first.reason, SelectionReason::Drawn);
    }

    #[test]
    fn draw_covers_the_whole_pool_across_seeds() {
        let config = ExperimentationConfig::default();
        let mut seen = std::collections::BTreeSet::new();
        for i in 0..64u128 {
            let seed = Uuid::from_u128(i);
            let decision = select_variant(
                &config,
                LOCATION_CONTEXT_ASSEMBLY,
                &["a", "b"],
                "default",
                seed,
            )
            .expect("draw should succeed");
            seen.insert(decision.variant);
        }
        assert!(
            seen.contains("default") && seen.contains("a") && seen.contains("b"),
            "every pool member should be reachable: {seen:?}"
        );
    }
}

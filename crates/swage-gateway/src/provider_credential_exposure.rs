//! The audit record of a provider credential that travels in plaintext.
//!
//! The exposure is a fact of the provider's configuration, judged when the
//! provider is built (`swage_provider::credential_transport`). This module
//! writes what the gateway did about it — warned and sent, or refused —
//! once per provider for each exposure and decision: the record is the one
//! warning the warn policy promises, and a rebuild of the same provider (a
//! settings save, a turn with its own route, a restart) finds the record
//! already there and writes nothing.

use swage_core::StorageError;
use swage_provider::PlaintextExposure;
use swage_storage::store::Storage;
use swage_storage::{AuditEventFilter, NewAuditEvent};
use uuid::Uuid;

/// The event kind under which the record lists these.
pub(crate) const EVENT_KIND: &str = "provider_credential_exposure";
/// The entity kind: the provider entry, by its id.
pub(crate) const ENTITY_KIND: &str = "provider";

/// What the gateway did with the exposure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExposureDecision {
    /// Requests are sent; this record is the warning.
    Warned,
    /// No request is sent.
    Refused,
}

impl ExposureDecision {
    fn as_str(self) -> &'static str {
        match self {
            Self::Warned => "warned",
            Self::Refused => "refused",
        }
    }
}

/// Record the decision unless the provider's latest record already carries
/// this exposure and decision. Returns whether a record was written.
///
/// The read and the write are two storage calls, so two gateways building
/// the same provider at once could each write; a second record of the same
/// fact is a duplicate in the trail, not a wrong one.
pub(crate) async fn record_once(
    storage: &dyn Storage,
    provider_id: &str,
    exposure: &PlaintextExposure,
    decision: ExposureDecision,
) -> Result<bool, StorageError> {
    let latest = storage
        .list_audit_events_filtered(AuditEventFilter {
            entity_kind: Some(ENTITY_KIND.into()),
            entity_id: Some(provider_id.into()),
            event_kind: Some(EVENT_KIND.into()),
            limit: 1,
            ..Default::default()
        })
        .await?;
    if latest.first().is_some_and(|event| {
        event.metadata["base_url"] == exposure.base_url
            && event.metadata["decision"] == decision.as_str()
    }) {
        return Ok(false);
    }

    let summary = match decision {
        ExposureDecision::Warned => exposure.warning(provider_id),
        ExposureDecision::Refused => exposure.refusal_reason(provider_id),
    };
    storage
        .create_audit_event(NewAuditEvent {
            id: Uuid::new_v4().to_string(),
            session_id: None,
            entity_kind: ENTITY_KIND.into(),
            entity_id: provider_id.into(),
            event_kind: EVENT_KIND.into(),
            summary,
            metadata: serde_json::json!({
                "base_url": exposure.base_url,
                "host": exposure.host,
                "decision": decision.as_str(),
            }),
        })
        .await?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use swage_storage::SqliteStorage;
    use swage_storage::store::AuditStore;

    use super::*;

    fn exposure(base_url: &str) -> PlaintextExposure {
        swage_provider::ProviderConfig::new("secret", "model", base_url)
            .plaintext_exposure()
            .expect("a remote http base URL exposes the credential")
    }

    async fn recorded(storage: &SqliteStorage) -> Vec<swage_storage::AuditEvent> {
        storage
            .list_audit_events_filtered(AuditEventFilter {
                event_kind: Some(EVENT_KIND.into()),
                limit: 10,
                ..Default::default()
            })
            .await
            .unwrap()
    }

    /// One record per provider for each exposure and decision: a repeat is
    /// not written, a changed decision or a changed host is.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_repeated_exposure_is_recorded_once_and_a_changed_one_again() {
        let storage = Arc::new(SqliteStorage::in_memory().unwrap());
        let lan = exposure("http://10.0.0.5:8080/v1");

        assert!(
            record_once(storage.as_ref(), "lan", &lan, ExposureDecision::Warned)
                .await
                .unwrap()
        );
        assert!(
            !record_once(storage.as_ref(), "lan", &lan, ExposureDecision::Warned)
                .await
                .unwrap()
        );
        assert_eq!(recorded(&storage).await.len(), 1);

        assert!(
            record_once(storage.as_ref(), "lan", &lan, ExposureDecision::Refused)
                .await
                .unwrap()
        );
        let other = exposure("http://10.0.0.6:8080/v1");
        assert!(
            record_once(storage.as_ref(), "lan", &other, ExposureDecision::Refused)
                .await
                .unwrap()
        );
        assert!(
            record_once(storage.as_ref(), "other", &lan, ExposureDecision::Warned)
                .await
                .unwrap()
        );

        let events = recorded(&storage).await;
        assert_eq!(events.len(), 4);
        let newest = &events[0];
        assert_eq!(newest.entity_kind, ENTITY_KIND);
        assert_eq!(newest.entity_id, "other");
        assert_eq!(newest.session_id, None);
        assert_eq!(newest.metadata["host"], "10.0.0.5");
        assert_eq!(newest.metadata["decision"], "warned");
        assert!(newest.summary.contains("'other'"));
        assert!(!newest.summary.contains("secret"));
        assert!(
            events
                .iter()
                .all(|event| !event.metadata.to_string().contains("secret"))
        );
    }
}

//! Registered secret values masked out of what the gateway answers its
//! clients with: every JSON answer under `/api/` and every frame of a
//! conversation's socket.
//!
//! An answer is masked as a record is, with one difference: a field named as
//! a key (`secret_masking::is_key_field`) keeps its value when the store
//! holds that value in one of its own key columns, so a topic named with a
//! registered word still answers with the slug its links use. A field of the
//! same name holding anything else — what a tool printed, what a model or a
//! person wrote — is masked like the rest.

use swage_core::secret_masking::{answer_key_candidates, mask_answer, mask_record_text};
use swage_storage::store::SecretStore;

/// Mask every registered value in `text`, an answer the gateway is about to
/// send. Returns whether anything was replaced.
pub async fn mask_answer_text(store: &dyn SecretStore, text: &mut String) -> bool {
    let values = store.registered_secret_values().current();
    if values.is_empty() {
        return false;
    }
    let trimmed = text.trim_start();
    let Some(mut answer) = (trimmed.starts_with('{') || trimmed.starts_with('['))
        .then(|| serde_json::from_str::<serde_json::Value>(text).ok())
        .flatten()
    else {
        return mask_record_text(text, &values);
    };
    let candidates = answer_key_candidates(&answer, &values);
    let keys = if candidates.is_empty() {
        Default::default()
    } else {
        // Without the store's answer no field is taken for a key, so the
        // answer is masked whole rather than sent holding a value.
        store
            .stored_keys_among(candidates)
            .await
            .unwrap_or_else(|error| {
                tracing::warn!(%error, "answer keys could not be checked against the store");
                Default::default()
            })
    };
    let changed = mask_answer(&mut answer, &values, &keys);
    if changed {
        *text = answer.to_string();
    }
    changed
}

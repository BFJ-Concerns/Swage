//! Which session and turn a completion is made for.
//!
//! A provider call carries no session of its own, so whoever runs a turn
//! sets [`COMPLETION_ATTRIBUTION`] around it, and a provider that records
//! its completions reads it when a call starts. A completion made outside
//! any such scope has no attribution.

use uuid::Uuid;

/// The session a completion serves and, where one is known, its turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompletionAttribution {
    pub session_id: Uuid,
    pub turn_id: Option<Uuid>,
}

tokio::task_local! {
    /// Set around a turn's execution so that every completion the turn makes
    /// on its own task — its rounds and any summary it asks for on the way —
    /// names its session and turn.
    pub static COMPLETION_ATTRIBUTION: CompletionAttribution;
}

/// The attribution of the turn running on this task, if any.
pub fn current_attribution() -> Option<CompletionAttribution> {
    COMPLETION_ATTRIBUTION
        .try_with(|attribution| *attribution)
        .ok()
}

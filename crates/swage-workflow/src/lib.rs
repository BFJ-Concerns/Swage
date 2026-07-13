//! Deterministic JavaScript orchestration with journal-backed replay.

mod engine;
mod host;
mod model;
mod recovery;

use sha2::{Digest, Sha256};

pub use engine::{EngineLimits, WorkflowEngine, WorkflowEngineError};
pub use host::{AgentCallRequest, AgentCallResult, EffectPayload, EffectReceipt, WorkflowHost};
pub use model::EffectKind;
pub use recovery::{EffectRecoveryHost, recover_started_effect};

/// Hash a canonical JSON host-call request for durable replay matching.
pub fn journal_args_hash(request: &serde_json::Value) -> String {
    let bytes = serde_json::to_vec(request).expect("JSON values always serialise");
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

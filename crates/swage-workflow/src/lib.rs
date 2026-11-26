//! Deterministic JavaScript orchestration with journal-backed replay.

mod engine;
mod host;
mod model;
mod recovery;

#[cfg(any(test, feature = "test-fixtures"))]
pub mod fixtures;

use sha2::{Digest, Sha256};

pub use engine::{
    EngineLimits, ScriptCapabilities, ScriptMeta, WorkflowEngine, WorkflowEngineError,
    parse_workflow, script_meta,
};
pub use host::{
    AgentCallRequest, AgentCallResult, AgentSessionRef, AwaitEventRequest, EffectPayload,
    EffectReceipt, EventDelivery, GateRequest, HeldWorkerOpen, HoldRequest,
    ResolvedWorkerPlacement, RunEventPost, WorkerLiveness, WorkflowCallRequest, WorkflowHost,
    WorkflowHostError, WorktreeCompletion,
};
pub use model::EffectKind;
pub use recovery::{EffectRecoveryHost, recover_started_effect};

/// Runtime version recorded on durable workflow runs. The workspace takes any
/// compatible rquickjs release; the lockfile fixes the one built, and a test
/// holds this string to it.
pub const QUICKJS_ENGINE_VERSION: &str = "rquickjs-0.12.2";

/// Hash a canonical JSON host-call request for durable replay matching.
pub fn journal_args_hash(request: &serde_json::Value) -> String {
    let bytes = serde_json::to_vec(request).expect("JSON values always serialise");
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::QUICKJS_ENGINE_VERSION;

    /// The version a run records is the rquickjs the workspace lockfile
    /// resolves, so a lockfile update cannot leave runs naming the old one.
    #[test]
    fn the_recorded_engine_version_is_the_locked_rquickjs() {
        let lock = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../Cargo.lock"));
        let locked = lock
            .split("[[package]]")
            .find(|entry| entry.contains("\nname = \"rquickjs\"\n"))
            .and_then(|entry| {
                entry
                    .lines()
                    .find_map(|line| line.strip_prefix("version = \""))
                    .and_then(|version| version.strip_suffix('"'))
            })
            .expect("Cargo.lock resolves rquickjs");
        assert_eq!(QUICKJS_ENGINE_VERSION, format!("rquickjs-{locked}"));
    }
}

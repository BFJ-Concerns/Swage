//! Deterministic JavaScript orchestration with journal-backed replay.

mod engine;
mod host;
mod host_calls;
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

/// The engine identity recorded on durable workflow runs.
pub use swage_isolate::{Termination, engine_version};

/// Hash a canonical JSON host-call request for durable replay matching.
///
/// Object keys are hashed in sorted order whatever order the request was
/// built in, so the hash depends on the request's content alone — never on
/// how `serde_json` happens to order a map.
pub fn journal_args_hash(request: &serde_json::Value) -> String {
    let bytes = serde_json::to_vec(&canonical(request)).expect("JSON values always serialise");
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn canonical(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            serde_json::Value::Object(
                keys.into_iter()
                    .map(|key| (key.clone(), canonical(&map[key])))
                    .collect(),
            )
        }
        serde_json::Value::Array(items) => {
            serde_json::Value::Array(items.iter().map(canonical).collect())
        }
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_replay_hash_ignores_the_order_keys_were_inserted_in() {
        let mut first = serde_json::Map::new();
        first.insert("prompt".into(), serde_json::json!("draft"));
        first.insert(
            "options".into(),
            serde_json::json!({"model": "m", "effort": "low"}),
        );
        let mut second = serde_json::Map::new();
        second.insert(
            "options".into(),
            serde_json::json!({"effort": "low", "model": "m"}),
        );
        second.insert("prompt".into(), serde_json::json!("draft"));
        assert_eq!(
            journal_args_hash(&serde_json::Value::Object(first)),
            journal_args_hash(&serde_json::Value::Object(second))
        );
        assert_ne!(
            journal_args_hash(&serde_json::json!({"prompt": "draft"})),
            journal_args_hash(&serde_json::json!({"prompt": "final"}))
        );
    }
}

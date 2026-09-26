//! Override-friendly constructors for the store's input and record types,
//! for tests in this crate and its dependants.
//!
//! Each constructor returns a complete, valid value with neutral defaults; a
//! test overrides only the fields it is about, through struct update syntax:
//!
//! ```ignore
//! let input = NewSession {
//!     agent_id: "reviewer".into(),
//!     ..new_session(topic_id)
//! };
//! ```
//!
//! The rule this buys: adding a column touches the type, the store, and this
//! module — never every test that happens to spell the type. A test about a
//! field keeps spelling that field; a test about something else never does.
//!
//! Arguments are the fields with no neutral default — an owning id the
//! caller must already hold. Everything else defaults here, and a default
//! is neutral: the value a caller who does not care about the field would
//! pick, never one that steers a query or a rendering.
//!
//! Exposed to dependants through the `test-fixtures` feature, which only a
//! `[dev-dependencies]` entry enables — the module never reaches a
//! production build.

use chrono::Utc;
use uuid::Uuid;

use crate::model::{NewSession, NewWorkflowRun, Session, SessionOrigin};

/// The agent id a fixture session runs under when the test does not say.
pub const FIXTURE_AGENT_ID: &str = "gateway";

/// A direct session under `topic_id`, owned by [`FIXTURE_AGENT_ID`], with
/// no metadata and no active workspace attachment.
pub fn new_session(topic_id: Uuid) -> NewSession {
    NewSession {
        topic_id,
        agent_id: FIXTURE_AGENT_ID.to_string(),
        origin: SessionOrigin::Direct,
        metadata: None,
        active_workspace_attachment_id: None,
    }
}

/// A persisted-shaped session record under `topic_id`, as a store would
/// return it: fresh id, timestamps now, not archived, no selected model.
/// For mocks and projections that hold records without a store.
pub fn session(topic_id: Uuid) -> Session {
    let now = Utc::now();
    Session {
        id: Uuid::new_v4(),
        topic_id,
        agent_id: FIXTURE_AGENT_ID.to_string(),
        origin: SessionOrigin::Direct,
        metadata: None,
        selected_model_id: None,
        active_workspace_attachment_id: None,
        archived_at: None,
        created_at: now,
        updated_at: now,
    }
}

/// A top-level, inline-source workflow run under `topic_id` with a unique
/// source hash, null args, an empty budget, and nothing bound or inherited.
pub fn new_workflow_run(topic_id: Uuid) -> NewWorkflowRun {
    NewWorkflowRun {
        topic_id,
        parent_run_id: None,
        parent_seq: None,
        dispatching_session_id: None,
        script_version_id: None,
        inline_workflow_id: None,
        inline_workflow_name: None,
        inline_workflow_version: None,
        carrying_skill: None,
        task: None,
        source: Some("return null".into()),
        source_hash: Uuid::new_v4().to_string(),
        args: serde_json::Value::Null,
        budget: serde_json::json!({}),
        engine_version: "test".into(),
        dispatch_bound_agent_ids: vec![],
        dispatch_bound_task_definition_ids: vec![],
        workflow_call_tools: None,
        inherited_worker_placement: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_override_keeps_every_other_default() {
        let topic_id = Uuid::new_v4();
        let input = NewSession {
            agent_id: "reviewer".into(),
            ..new_session(topic_id)
        };
        assert_eq!(input.agent_id, "reviewer");
        assert_eq!(input.topic_id, topic_id);
        assert_eq!(input.origin, SessionOrigin::Direct);
        assert!(input.metadata.is_none());
    }

    #[test]
    fn each_workflow_run_fixture_has_its_own_source_hash() {
        let topic_id = Uuid::new_v4();
        assert_ne!(
            new_workflow_run(topic_id).source_hash,
            new_workflow_run(topic_id).source_hash
        );
    }
}

//! The turn graph as the gateway keeps and executes it.
//!
//! The graph is the versioned declaration of the turn pipeline's steps and
//! their order (`swage_protocol::client::turn_graph`). Revision 0 is the
//! shipped graph — every slot the kernel's own, no hooks, no branches —
//! and serves until a revision that validates is activated. A proposed
//! revision is validated against what is installed now (the plugins and
//! the slots and hooks they provide, the saved workflows), recorded
//! whether it validates or not — a failed one with why, the last good one
//! still serving — and activated when it does. Every revision is
//! inspectable and any two are diffable. The coordinator asks the active
//! revision which filler serves each slot and which hooks are attached at
//! each point; enforcement — grants, approvals, the audit at the effect
//! boundary — never depends on a revision.

use std::sync::{Arc, RwLock};

use swage_core::StorageError;
use swage_protocol::client::turn_graph::{
    HookAttachment, HookMode, HookPoint, RevisionActivation, StepFiller, TurnGraph, TurnGraphDiff,
    TurnGraphResponse, TurnGraphRevision, TurnSlot, TurnStep,
};
use swage_storage::model::{NewAuditEvent, NewTurnGraphRevision, TurnGraphRevisionRecord};
use swage_storage::store::{Storage, TopicStore};
use uuid::Uuid;

use crate::plugin::PluginService;

#[derive(Debug, thiserror::Error)]
pub enum TurnGraphError {
    /// The graph does not validate against what is installed; nothing
    /// changed what serves.
    #[error("{0}")]
    Refused(String),
    #[error("no turn graph revision {0} is recorded")]
    NotFound(u32),
    #[error("{0}")]
    Storage(#[from] StorageError),
}

/// The revision serving now, parsed once.
#[derive(Debug, Clone)]
pub struct ActiveGraph {
    pub revision: u32,
    pub graph: TurnGraph,
}

/// The graph's revisions, live in the gateway process.
pub struct TurnGraphService {
    storage: Arc<dyn Storage>,
    topics: Arc<dyn TopicStore>,
    plugins: Arc<PluginService>,
    revisions: RwLock<Vec<TurnGraphRevisionRecord>>,
    active: RwLock<Arc<ActiveGraph>>,
    /// Proposals and activations run one at a time: the revision number and
    /// the activation swap are one decision.
    change_lock: tokio::sync::Mutex<()>,
}

impl TurnGraphService {
    /// A service serving the shipped graph; [`Self::load`] brings the
    /// recorded revisions up.
    pub fn new(
        storage: Arc<dyn Storage>,
        topics: Arc<dyn TopicStore>,
        plugins: Arc<PluginService>,
    ) -> Self {
        Self {
            storage,
            topics,
            plugins,
            revisions: RwLock::new(Vec::new()),
            active: RwLock::new(Arc::new(ActiveGraph {
                revision: 0,
                graph: TurnGraph::shipped(),
            })),
            change_lock: tokio::sync::Mutex::new(()),
        }
    }

    /// Bring the recorded revisions up. An install with none records the
    /// shipped graph as revision 0, active. An active record whose graph no
    /// longer reads serves the shipped graph, with a warning.
    pub async fn load(&self) -> Result<(), StorageError> {
        let _changing = self.change_lock.lock().await;
        let mut revisions = self.storage.list_turn_graph_revisions().await?;
        if revisions.is_empty() {
            let shipped = self
                .storage
                .record_turn_graph_revision(NewTurnGraphRevision {
                    revision: 0,
                    graph: serde_json::to_value(TurnGraph::shipped())
                        .expect("the shipped graph serialises"),
                    activation: "active".into(),
                    failure: None,
                })
                .await?;
            revisions.push(shipped);
        }
        let active = revisions
            .iter()
            .find(|record| record.activation == "active")
            .map(|record| {
                let graph = serde_json::from_value::<TurnGraph>(record.graph.clone())
                    .unwrap_or_else(|error| {
                        tracing::warn!(revision = record.revision, %error, "the active turn graph revision does not read; the shipped graph serves");
                        TurnGraph::shipped()
                    });
                ActiveGraph {
                    revision: record.revision,
                    graph,
                }
            })
            .unwrap_or_else(|| ActiveGraph {
                revision: 0,
                graph: TurnGraph::shipped(),
            });
        *self.write_revisions() = revisions;
        *self.write_active() = Arc::new(active);
        Ok(())
    }

    /// The revision serving now.
    pub fn active(&self) -> Arc<ActiveGraph> {
        Arc::clone(
            &self
                .active
                .read()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
        )
    }

    /// What serves `slot` under the active revision: the kernel's own
    /// implementation unless a revision filled the slot otherwise.
    pub fn filler_for(&self, slot: TurnSlot) -> StepFiller {
        self.active()
            .graph
            .steps
            .iter()
            .find(|step| step.slot == slot)
            .map(|step| step.filler.clone())
            .unwrap_or(StepFiller::Builtin)
    }

    /// The hooks attached at `point` under the active revision, in order.
    pub fn hooks_at(&self, point: HookPoint) -> Vec<HookAttachment> {
        self.active()
            .graph
            .hooks
            .iter()
            .filter(|hook| hook.point == point)
            .cloned()
            .collect()
    }

    /// The branches shadowing the live step `step_id` under the active
    /// revision.
    pub fn branches_from(
        &self,
        step_id: &str,
    ) -> Vec<swage_protocol::client::turn_graph::TurnBranch> {
        self.active()
            .graph
            .branches
            .iter()
            .filter(|branch| branch.from == step_id)
            .cloned()
            .collect()
    }

    /// The graph as it stands: the active revision and every one recorded.
    pub fn list(&self) -> TurnGraphResponse {
        TurnGraphResponse {
            active: self.active().revision,
            revisions: self.read_revisions().iter().map(revision_view).collect(),
        }
    }

    pub fn revision(&self, revision: u32) -> Option<TurnGraphRevision> {
        self.read_revisions()
            .iter()
            .find(|record| record.revision == revision)
            .map(revision_view)
    }

    /// What changed from revision `from` to revision `to`.
    pub fn diff(&self, from: u32, to: u32) -> Result<TurnGraphDiff, TurnGraphError> {
        let before = self.revision(from).ok_or(TurnGraphError::NotFound(from))?;
        let after = self.revision(to).ok_or(TurnGraphError::NotFound(to))?;
        Ok(TurnGraphDiff::between(
            from,
            &before.graph,
            to,
            &after.graph,
        ))
    }

    /// Propose `graph` as the next revision: validated against what is
    /// installed now, recorded either way, and activated when it validates.
    /// The record comes back in both cases — a failed revision carries why,
    /// and the revision that served before keeps serving.
    pub async fn propose(
        &self,
        graph: TurnGraph,
        session_id: Option<Uuid>,
    ) -> Result<TurnGraphRevision, TurnGraphError> {
        let _changing = self.change_lock.lock().await;
        let next = self
            .read_revisions()
            .iter()
            .map(|record| record.revision)
            .max()
            .map_or(0, |last| last + 1);
        let verdict = self.validate(&graph).await;
        let graph_json = serde_json::to_value(&graph)
            .map_err(|error| TurnGraphError::Refused(error.to_string()))?;
        let record = self
            .storage
            .record_turn_graph_revision(NewTurnGraphRevision {
                revision: next,
                graph: graph_json,
                activation: if verdict.is_ok() { "active" } else { "failed" }.into(),
                failure: verdict.as_ref().err().cloned(),
            })
            .await?;
        {
            let mut revisions = self.write_revisions();
            if verdict.is_ok() {
                for earlier in revisions.iter_mut() {
                    if earlier.activation == "active" {
                        earlier.activation = "superseded".into();
                    }
                }
            }
            revisions.push(record.clone());
        }
        match &verdict {
            Ok(()) => {
                *self.write_active() = Arc::new(ActiveGraph {
                    revision: next,
                    graph: graph.clone(),
                });
                self.record_event(
                    session_id,
                    next,
                    "turn_graph_revision_activated",
                    format!("Turn graph revision {next} validated and activated"),
                    serde_json::json!({ "revision": next, "graph": graph }),
                )
                .await;
            }
            Err(reason) => {
                self.record_event(
                    session_id,
                    next,
                    "turn_graph_revision_failed",
                    format!("Turn graph revision {next} failed validation: {reason}"),
                    serde_json::json!({ "revision": next, "reason": reason, "serving": self.active().revision }),
                )
                .await;
            }
        }
        Ok(revision_view(&record))
    }

    /// Make a recorded revision the one that serves, re-validated against
    /// what is installed now.
    pub async fn activate(
        &self,
        revision: u32,
        session_id: Option<Uuid>,
    ) -> Result<TurnGraphRevision, TurnGraphError> {
        let _changing = self.change_lock.lock().await;
        let candidate = self
            .revision(revision)
            .ok_or(TurnGraphError::NotFound(revision))?;
        self.validate(&candidate.graph)
            .await
            .map_err(TurnGraphError::Refused)?;
        self.storage.activate_turn_graph_revision(revision).await?;
        {
            let mut revisions = self.write_revisions();
            for record in revisions.iter_mut() {
                if record.activation == "active" {
                    record.activation = "superseded".into();
                }
                if record.revision == revision {
                    record.activation = "active".into();
                    record.failure = None;
                }
            }
        }
        *self.write_active() = Arc::new(ActiveGraph {
            revision,
            graph: candidate.graph.clone(),
        });
        self.record_event(
            session_id,
            revision,
            "turn_graph_revision_activated",
            format!("Turn graph revision {revision} activated"),
            serde_json::json!({ "revision": revision }),
        )
        .await;
        self.revision(revision)
            .ok_or(TurnGraphError::NotFound(revision))
    }

    /// Whether `graph` can serve now: every rule names what it refuses.
    async fn validate(&self, graph: &TurnGraph) -> Result<(), String> {
        if graph.steps.is_empty() {
            return Err("the live path has no steps".into());
        }
        let mut ids = std::collections::HashSet::new();
        let mut slots = std::collections::HashSet::new();
        for step in &graph.steps {
            if !ids.insert(step.id.as_str()) {
                return Err(format!("step id '{}' is used twice", step.id));
            }
            if !slots.insert(step.slot) {
                return Err(format!(
                    "slot '{}' appears twice on the live path",
                    step.slot.as_str()
                ));
            }
            self.validate_filler(step).await?;
        }
        for hook in &graph.hooks {
            let declared = self
                .plugins
                .hook_mode(&hook.plugin, hook.point.as_str())
                .ok_or_else(|| {
                    format!(
                        "hook at '{}': plugin '{}' is not installed and serving with a hook there",
                        hook.point.as_str(),
                        hook.plugin
                    )
                })?;
            if mode_rank(hook.mode) > mode_rank_of(&declared) {
                return Err(format!(
                    "hook at '{}': plugin '{}' declares mode '{}', which does not allow '{}'",
                    hook.point.as_str(),
                    hook.plugin,
                    declared,
                    mode_name(hook.mode)
                ));
            }
        }
        let mut branch_ids = std::collections::HashSet::new();
        for branch in &graph.branches {
            if !branch_ids.insert(branch.id.as_str()) {
                return Err(format!("branch id '{}' is used twice", branch.id));
            }
            if !graph.steps.iter().any(|step| step.id == branch.from) {
                return Err(format!(
                    "branch '{}' runs from '{}', which is not a step of the live path",
                    branch.id, branch.from
                ));
            }
            if branch.steps.is_empty() {
                return Err(format!("branch '{}' has no steps", branch.id));
            }
            for step in &branch.steps {
                if !ids.insert(step.id.as_str()) {
                    return Err(format!("step id '{}' is used twice", step.id));
                }
                self.validate_filler(step).await?;
            }
        }
        Ok(())
    }

    async fn validate_filler(&self, step: &TurnStep) -> Result<(), String> {
        match &step.filler {
            StepFiller::Builtin => Ok(()),
            StepFiller::Plugin { name } => {
                if !self.plugins.provides_slot(name, step.slot.as_str()) {
                    return Err(format!(
                        "step '{}': plugin '{}' is not installed and serving with slot '{}'",
                        step.id,
                        name,
                        step.slot.as_str()
                    ));
                }
                Ok(())
            }
            StepFiller::Workflow { name } => {
                let general = self
                    .topics
                    .general_scope()
                    .await
                    .map_err(|error| error.to_string())?;
                let scripts = self
                    .storage
                    .list_workflow_scripts(general.id)
                    .await
                    .map_err(|error| error.to_string())?;
                if !scripts.iter().any(|script| &script.name == name) {
                    return Err(format!(
                        "step '{}': no saved workflow named '{}' in the general scope",
                        step.id, name
                    ));
                }
                Ok(())
            }
        }
    }

    async fn record_event(
        &self,
        session_id: Option<Uuid>,
        revision: u32,
        kind: &str,
        summary: String,
        metadata: serde_json::Value,
    ) {
        if let Err(error) = self
            .storage
            .create_audit_event(NewAuditEvent {
                id: Uuid::new_v4().to_string(),
                session_id,
                entity_kind: "turn_graph".to_owned(),
                entity_id: revision.to_string(),
                event_kind: kind.to_owned(),
                summary,
                metadata,
            })
            .await
        {
            tracing::warn!(%error, kind, "turn graph event could not be recorded");
        }
    }

    fn read_revisions(&self) -> std::sync::RwLockReadGuard<'_, Vec<TurnGraphRevisionRecord>> {
        self.revisions
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn write_revisions(&self) -> std::sync::RwLockWriteGuard<'_, Vec<TurnGraphRevisionRecord>> {
        self.revisions
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn write_active(&self) -> std::sync::RwLockWriteGuard<'_, Arc<ActiveGraph>> {
        self.active
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

fn revision_view(record: &TurnGraphRevisionRecord) -> TurnGraphRevision {
    TurnGraphRevision {
        revision: record.revision,
        graph: serde_json::from_value(record.graph.clone()).unwrap_or_else(|_| TurnGraph {
            steps: Vec::new(),
            hooks: Vec::new(),
            branches: Vec::new(),
        }),
        activation: match record.activation.as_str() {
            "active" => RevisionActivation::Active,
            "failed" => RevisionActivation::Failed,
            _ => RevisionActivation::Superseded,
        },
        failure: record.failure.clone(),
        created_at: record.created_at.to_rfc3339(),
    }
}

fn mode_rank(mode: HookMode) -> u8 {
    match mode {
        HookMode::Observe => 0,
        HookMode::Rewrite => 1,
        HookMode::Veto => 2,
    }
}

fn mode_rank_of(declared: &str) -> u8 {
    match declared {
        "rewrite" => 1,
        "veto" => 2,
        _ => 0,
    }
}

fn mode_name(mode: HookMode) -> &'static str {
    match mode {
        HookMode::Observe => "observe",
        HookMode::Rewrite => "rewrite",
        HookMode::Veto => "veto",
    }
}

/// The slot names the gateway has, as a plugin's manifest names them.
pub fn known_slot(name: &str) -> bool {
    TurnSlot::ALL.iter().any(|slot| slot.as_str() == name)
}

/// The hook points the gateway has, as a plugin's manifest names them.
pub fn known_hook_point(name: &str) -> bool {
    [
        HookPoint::MessageInbound,
        HookPoint::ToolCall,
        HookPoint::TurnStart,
        HookPoint::TurnEnd,
    ]
    .iter()
    .any(|point| point.as_str() == name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use swage_protocol::client::turn_graph::TurnBranch;
    use swage_storage::SqliteStorage;
    use swage_storage::model::NewWorkflowScript;
    use swage_storage::store::{AuditStore, WorkflowStore};

    fn write_plugin(root: &Path, name: &str, provides: &str, script: &str) {
        std::fs::create_dir_all(root).unwrap();
        std::fs::write(
            root.join("plugin.toml"),
            format!(
                r#"
                name = "{name}"
                version = "1"
                description = "A {name}."
                script = "plugin.js"
                grants = ["model"]
                {provides}
                "#
            ),
        )
        .unwrap();
        std::fs::write(root.join("plugin.js"), script).unwrap();
    }

    async fn service(dir: &Path) -> (TurnGraphService, Arc<SqliteStorage>, Arc<PluginService>) {
        let storage = Arc::new(SqliteStorage::in_memory().unwrap());
        let plugins = Arc::new(PluginService::new(
            Arc::clone(&storage) as Arc<dyn Storage>,
            dir.join("plugins"),
        ));
        let graph = TurnGraphService::new(
            Arc::clone(&storage) as Arc<dyn Storage>,
            Arc::clone(&storage) as Arc<dyn TopicStore>,
            Arc::clone(&plugins),
        );
        graph.load().await.unwrap();
        (graph, storage, plugins)
    }

    fn step(id: &str, slot: TurnSlot, filler: StepFiller) -> TurnStep {
        TurnStep {
            id: id.into(),
            slot,
            filler,
        }
    }

    fn with_succession(filler: StepFiller) -> TurnGraph {
        let mut graph = TurnGraph::shipped();
        graph
            .steps
            .iter_mut()
            .find(|step| step.slot == TurnSlot::Succession)
            .unwrap()
            .filler = filler;
        graph
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_install_starts_on_the_shipped_graph_as_revision_zero() {
        let dir = tempfile::tempdir().unwrap();
        let (graph, storage, _) = service(dir.path()).await;
        let listed = graph.list();
        assert_eq!(listed.active, 0);
        assert_eq!(listed.revisions.len(), 1);
        assert_eq!(listed.revisions[0].graph, TurnGraph::shipped());
        assert_eq!(graph.filler_for(TurnSlot::Succession), StepFiller::Builtin);
        assert!(graph.hooks_at(HookPoint::ToolCall).is_empty());
        // A second load reads the record back rather than seeding again.
        let again = TurnGraphService::new(
            Arc::clone(&storage) as Arc<dyn Storage>,
            Arc::clone(&storage) as Arc<dyn TopicStore>,
            Arc::new(PluginService::new(
                Arc::clone(&storage) as Arc<dyn Storage>,
                dir.path().join("plugins"),
            )),
        );
        again.load().await.unwrap();
        assert_eq!(again.list().revisions.len(), 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_invalid_revision_is_recorded_as_failed_and_the_prior_one_serves() {
        let dir = tempfile::tempdir().unwrap();
        let (graph, storage, _) = service(dir.path()).await;
        let proposed = graph
            .propose(
                with_succession(StepFiller::Plugin {
                    name: "absent".into(),
                }),
                None,
            )
            .await
            .expect("recorded");
        assert_eq!(proposed.revision, 1);
        assert_eq!(proposed.activation, RevisionActivation::Failed);
        assert!(
            proposed.failure.as_deref().unwrap().contains("absent"),
            "{proposed:?}"
        );
        assert_eq!(graph.active().revision, 0, "the shipped graph still serves");
        assert_eq!(graph.filler_for(TurnSlot::Succession), StepFiller::Builtin);
        let listed = graph.list();
        assert_eq!(listed.revisions[0].activation, RevisionActivation::Active);
        assert_eq!(listed.revisions[1].activation, RevisionActivation::Failed);
        let events = storage.list_audit_events().await.unwrap();
        assert!(
            events.iter().any(|event| event.entity_kind == "turn_graph"
                && event.entity_id == "1"
                && event.event_kind == "turn_graph_revision_failed"),
            "{events:?}"
        );

        let mut duplicate = TurnGraph::shipped();
        duplicate
            .steps
            .push(step("again", TurnSlot::Curator, StepFiller::Builtin));
        let refused = graph.propose(duplicate, None).await.unwrap();
        assert!(
            refused
                .failure
                .as_deref()
                .unwrap()
                .contains("appears twice")
        );
        let mut empty = TurnGraph::shipped();
        empty.steps.clear();
        let refused = graph.propose(empty, None).await.unwrap();
        assert!(refused.failure.as_deref().unwrap().contains("no steps"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_plugin_that_provides_the_slot_fills_it_and_a_later_revision_can_be_rolled_back() {
        let dir = tempfile::tempdir().unwrap();
        let (graph, _, plugins) = service(dir.path()).await;
        write_plugin(
            &dir.path().join("summariser"),
            "summariser",
            "[[provides.slots]]\nname = \"succession\"",
            r#"swage.provide.slot("succession", (input) => ({ summary: "short" }));"#,
        );
        plugins
            .install(&dir.path().join("summariser"), &[], None)
            .await
            .expect("installs");
        let filled = graph
            .propose(
                with_succession(StepFiller::Plugin {
                    name: "summariser".into(),
                }),
                None,
            )
            .await
            .unwrap();
        assert_eq!(filled.activation, RevisionActivation::Active);
        assert_eq!(graph.active().revision, 1);
        assert_eq!(
            graph.filler_for(TurnSlot::Succession),
            StepFiller::Plugin {
                name: "summariser".into()
            }
        );
        let diff = graph.diff(0, 1).unwrap();
        assert_eq!(diff.steps_refilled.len(), 1);
        assert_eq!(diff.steps_refilled[0].id, "succession");
        assert_eq!(diff.steps_refilled[0].before, StepFiller::Builtin);
        assert!(diff.steps_added.is_empty() && diff.steps_removed.is_empty());

        // A plugin filling a slot it does not provide is refused by name.
        let wrong = graph
            .propose(
                {
                    let mut graph = TurnGraph::shipped();
                    graph
                        .steps
                        .iter_mut()
                        .find(|s| s.slot == TurnSlot::Curator)
                        .unwrap()
                        .filler = StepFiller::Plugin {
                        name: "summariser".into(),
                    };
                    graph
                },
                None,
            )
            .await
            .unwrap();
        assert_eq!(wrong.activation, RevisionActivation::Failed);
        assert!(wrong.failure.as_deref().unwrap().contains("slot 'curator'"));
        assert_eq!(graph.active().revision, 1);

        let back = graph.activate(0, None).await.expect("rolls back");
        assert_eq!(back.activation, RevisionActivation::Active);
        assert_eq!(graph.active().revision, 0);
        assert_eq!(
            graph.revision(1).unwrap().activation,
            RevisionActivation::Superseded
        );
        assert!(matches!(
            graph.activate(9, None).await,
            Err(TurnGraphError::NotFound(9))
        ));
        assert!(
            matches!(
                graph.activate(2, None).await,
                Err(TurnGraphError::Refused(_))
            ),
            "a failed revision stays refused"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn hooks_branches_and_workflow_fillers_validate_against_what_is_installed() {
        let dir = tempfile::tempdir().unwrap();
        let (graph, storage, plugins) = service(dir.path()).await;
        write_plugin(
            &dir.path().join("guard"),
            "guard",
            "[[provides.hooks]]\npoint = \"tool_call\"\nmode = \"rewrite\"",
            r#"swage.provide.hook("tool_call", (event) => ({ action: "continue" }));"#,
        );
        plugins
            .install(&dir.path().join("guard"), &[], None)
            .await
            .expect("installs");
        let general = storage.general_scope().await.unwrap();
        storage
            .create_workflow_script(NewWorkflowScript {
                topic_id: general.id,
                name: "curate".into(),
                description: "Curates.".into(),
                source: "export const meta = { name: 'curate' };".into(),
                delivery_grant: None,
                captured_sources_grant: false,
                authoring_session_id: None,
            })
            .await
            .unwrap();

        let mut proposed = TurnGraph::shipped();
        proposed.hooks.push(HookAttachment {
            point: HookPoint::ToolCall,
            plugin: "guard".into(),
            mode: HookMode::Rewrite,
        });
        proposed
            .steps
            .iter_mut()
            .find(|s| s.slot == TurnSlot::Curator)
            .unwrap()
            .filler = StepFiller::Workflow {
            name: "curate".into(),
        };
        proposed.branches.push(TurnBranch {
            id: "trial".into(),
            from: "succession".into(),
            steps: vec![step(
                "trial-succession",
                TurnSlot::Succession,
                StepFiller::Builtin,
            )],
        });
        let recorded = graph.propose(proposed, None).await.unwrap();
        assert_eq!(
            recorded.activation,
            RevisionActivation::Active,
            "{recorded:?}"
        );
        assert_eq!(graph.hooks_at(HookPoint::ToolCall).len(), 1);
        assert_eq!(graph.branches_from("succession").len(), 1);
        let diff = graph.diff(0, 1).unwrap();
        assert_eq!(diff.hooks_attached.len(), 1);
        assert_eq!(diff.branches_added.len(), 1);

        let mut too_strong = TurnGraph::shipped();
        too_strong.hooks.push(HookAttachment {
            point: HookPoint::ToolCall,
            plugin: "guard".into(),
            mode: HookMode::Veto,
        });
        let refused = graph.propose(too_strong, None).await.unwrap();
        assert!(
            refused
                .failure
                .as_deref()
                .unwrap()
                .contains("does not allow 'veto'"),
            "{refused:?}"
        );
        let mut unknown_workflow = TurnGraph::shipped();
        unknown_workflow
            .steps
            .iter_mut()
            .find(|s| s.slot == TurnSlot::Judge)
            .unwrap()
            .filler = StepFiller::Workflow {
            name: "missing".into(),
        };
        let refused = graph.propose(unknown_workflow, None).await.unwrap();
        assert!(
            refused
                .failure
                .as_deref()
                .unwrap()
                .contains("no saved workflow"),
            "{refused:?}"
        );
        let mut dangling = TurnGraph::shipped();
        dangling.branches.push(TurnBranch {
            id: "b".into(),
            from: "nowhere".into(),
            steps: vec![step("b1", TurnSlot::Judge, StepFiller::Builtin)],
        });
        let refused = graph.propose(dangling, None).await.unwrap();
        assert!(
            refused
                .failure
                .as_deref()
                .unwrap()
                .contains("not a step of the live path")
        );
    }
}

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
    HookMode, HookPoint, RevisionActivation, StepFiller, TurnGraph, TurnGraphDiff,
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
    /// A recorded revision's graph no longer reads as a declaration — the
    /// declaration's shape moved under it. Nothing is guessed in its place.
    #[error("turn graph revision {revision} does not read as a declaration: {reason}")]
    Unreadable { revision: u32, reason: String },
    #[error("{0}")]
    Storage(#[from] StorageError),
}

/// The revision serving now, parsed once. A turn takes one of these at its
/// start and reads every filler, hook and branch from it, so a revision
/// activated mid-turn changes the next turn, never the one under way.
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

    /// The graph as it stands: the active revision and every one recorded.
    pub fn list(&self) -> Result<TurnGraphResponse, TurnGraphError> {
        Ok(TurnGraphResponse {
            active: self.active().revision,
            revisions: self
                .read_revisions()
                .iter()
                .map(revision_view)
                .collect::<Result<Vec<_>, _>>()?,
        })
    }

    pub fn revision(&self, revision: u32) -> Result<TurnGraphRevision, TurnGraphError> {
        self.read_revisions()
            .iter()
            .find(|record| record.revision == revision)
            .map(revision_view)
            .unwrap_or(Err(TurnGraphError::NotFound(revision)))
    }

    /// What changed from revision `from` to revision `to`.
    pub fn diff(&self, from: u32, to: u32) -> Result<TurnGraphDiff, TurnGraphError> {
        let before = self.revision(from)?;
        let after = self.revision(to)?;
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
        let next = self.next_revision_number();
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
        revision_view(&record)
    }

    /// Make a recorded revision the one that serves, re-validated against
    /// what is installed now. A refusal is recorded as it is at `propose`:
    /// a new failed revision carrying the candidate's graph and why it
    /// cannot serve now, returned in the refused case, with the revision
    /// that served before still serving.
    pub async fn activate(
        &self,
        revision: u32,
        session_id: Option<Uuid>,
    ) -> Result<TurnGraphRevision, TurnGraphError> {
        let _changing = self.change_lock.lock().await;
        let candidate = self.revision(revision)?;
        if let Err(reason) = self.validate(&candidate.graph).await {
            let next = self.next_revision_number();
            let record = self
                .storage
                .record_turn_graph_revision(NewTurnGraphRevision {
                    revision: next,
                    graph: serde_json::to_value(&candidate.graph)
                        .map_err(|error| TurnGraphError::Refused(error.to_string()))?,
                    activation: "failed".into(),
                    failure: Some(format!(
                        "activation of revision {revision} refused: {reason}"
                    )),
                })
                .await?;
            self.write_revisions().push(record.clone());
            self.record_event(
                session_id,
                next,
                "turn_graph_revision_failed",
                format!("Turn graph revision {revision} could not be re-activated: {reason}"),
                serde_json::json!({ "revision": next, "candidate": revision, "reason": reason, "serving": self.active().revision }),
            )
            .await;
            return revision_view(&record);
        }
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
    }

    fn next_revision_number(&self) -> u32 {
        self.read_revisions()
            .iter()
            .map(|record| record.revision)
            .max()
            .map_or(0, |last| last + 1)
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
        let mut attachments = std::collections::HashSet::new();
        for hook in &graph.hooks {
            if !attachments.insert((hook.plugin.as_str(), hook.point)) {
                return Err(format!(
                    "hook at '{}': plugin '{}' is attached there twice",
                    hook.point.as_str(),
                    hook.plugin
                ));
            }
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
            let Some(from) = graph.steps.iter().find(|step| step.id == branch.from) else {
                return Err(format!(
                    "branch '{}' runs from '{}', which is not a step of the live path",
                    branch.id, branch.from
                ));
            };
            if !shadowed(from.slot) {
                return Err(format!(
                    "branch '{}' runs from '{}', and the kernel runs no shadow of slot '{}'",
                    branch.id,
                    branch.from,
                    from.slot.as_str()
                ));
            }
            let [step] = branch.steps.as_slice() else {
                return Err(format!(
                    "branch '{}' must carry exactly one step, the shadow of '{}'",
                    branch.id, branch.from
                ));
            };
            if step.slot != from.slot {
                return Err(format!(
                    "branch '{}' shadows '{}' with a step of slot '{}', not '{}'",
                    branch.id,
                    branch.from,
                    step.slot.as_str(),
                    from.slot.as_str()
                ));
            }
            if !ids.insert(step.id.as_str()) {
                return Err(format!("step id '{}' is used twice", step.id));
            }
            self.validate_filler(step).await?;
        }
        Ok(())
    }

    /// Whether `step`'s filler can serve: the kind its slot's kernel point
    /// takes, and the plugin or workflow it names installed now.
    async fn validate_filler(&self, step: &TurnStep) -> Result<(), String> {
        if !takes(step.slot, &step.filler) {
            return Err(format!(
                "step '{}': slot '{}' takes {} — the kernel's point there runs no {} filler",
                step.id,
                step.slot.as_str(),
                takes_description(step.slot),
                filler_kind(&step.filler)
            ));
        }
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

/// What the kernel's point for `slot` runs besides its own implementation:
/// the context-program point composes through a plugin's isolate on the
/// turn's thread; the succession point takes a plugin or a saved workflow;
/// the other points take no filler yet, and a declaration naming one for
/// them is refused rather than recorded as serving.
fn takes(slot: TurnSlot, filler: &StepFiller) -> bool {
    matches!(
        (slot, filler),
        (_, StepFiller::Builtin)
            | (TurnSlot::ContextProgram, StepFiller::Plugin { .. })
            | (
                TurnSlot::Succession,
                StepFiller::Plugin { .. } | StepFiller::Workflow { .. }
            )
    )
}

fn takes_description(slot: TurnSlot) -> &'static str {
    match slot {
        TurnSlot::ContextProgram => "the kernel's own or a plugin",
        TurnSlot::Succession => "the kernel's own, a plugin or a saved workflow",
        TurnSlot::ToolDispatch | TurnSlot::Curator | TurnSlot::Judge => "the kernel's own only",
    }
}

fn filler_kind(filler: &StepFiller) -> &'static str {
    match filler {
        StepFiller::Builtin => "builtin",
        StepFiller::Plugin { .. } => "plugin",
        StepFiller::Workflow { .. } => "workflow",
    }
}

/// The slots whose kernel point runs a branch's shadow beside the live
/// step.
fn shadowed(slot: TurnSlot) -> bool {
    matches!(slot, TurnSlot::Succession)
}

fn revision_view(record: &TurnGraphRevisionRecord) -> Result<TurnGraphRevision, TurnGraphError> {
    let graph = serde_json::from_value(record.graph.clone()).map_err(|error| {
        TurnGraphError::Unreadable {
            revision: record.revision,
            reason: error.to_string(),
        }
    })?;
    Ok(TurnGraphRevision {
        revision: record.revision,
        graph,
        activation: match record.activation.as_str() {
            "active" => RevisionActivation::Active,
            "failed" => RevisionActivation::Failed,
            _ => RevisionActivation::Superseded,
        },
        failure: record.failure.clone(),
        created_at: record.created_at.to_rfc3339(),
    })
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
    use swage_protocol::client::turn_graph::{HookAttachment, TurnBranch};
    use swage_storage::SqliteStorage;
    use swage_storage::model::NewWorkflowScript;
    use swage_storage::store::{AuditStore, TurnGraphStore, WorkflowStore};

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

    /// A service as the gateway starts it: the shipped plugins installed
    /// from their projection, so revision 0's shipped summariser validates.
    async fn service(dir: &Path) -> (TurnGraphService, Arc<SqliteStorage>, Arc<PluginService>) {
        let storage = Arc::new(SqliteStorage::in_memory().unwrap());
        let plugins = Arc::new(PluginService::new(
            Arc::clone(&storage) as Arc<dyn Storage>,
            dir.join("plugins"),
        ));
        crate::shipped_plugins::regenerate(dir).await.unwrap();
        let names: Vec<&str> = crate::shipped_plugins::SHIPPED_PLUGINS
            .iter()
            .map(|plugin| plugin.name)
            .collect();
        plugins
            .install_shipped(&crate::shipped_plugins::root(dir), &names, &[])
            .await;
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
        let listed = graph.list().unwrap();
        assert_eq!(listed.active, 0);
        assert_eq!(listed.revisions.len(), 1);
        assert_eq!(listed.revisions[0].graph, TurnGraph::shipped());
        assert_eq!(
            graph.active().graph.filler_for(TurnSlot::Succession),
            StepFiller::Plugin {
                name: swage_protocol::client::turn_graph::SHIPPED_SUCCESSION_SUMMARISER.into()
            },
            "the shipped graph names the shipped summariser"
        );
        assert!(
            graph
                .active()
                .graph
                .hooks_at(HookPoint::ToolCall)
                .is_empty()
        );
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
        assert_eq!(again.list().unwrap().revisions.len(), 1);
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
        assert_eq!(
            graph.active().graph.filler_for(TurnSlot::Succession),
            StepFiller::Plugin {
                name: swage_protocol::client::turn_graph::SHIPPED_SUCCESSION_SUMMARISER.into()
            }
        );
        let listed = graph.list().unwrap();
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
        let (graph, storage, plugins) = service(dir.path()).await;
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
            graph.active().graph.filler_for(TurnSlot::Succession),
            StepFiller::Plugin {
                name: "summariser".into()
            }
        );
        let diff = graph.diff(0, 1).unwrap();
        assert_eq!(diff.steps_refilled.len(), 1);
        assert_eq!(diff.steps_refilled[0].id, "succession");
        assert_eq!(
            diff.steps_refilled[0].before,
            StepFiller::Plugin {
                name: swage_protocol::client::turn_graph::SHIPPED_SUCCESSION_SUMMARISER.to_owned()
            },
            "the shipped graph fills succession with the shipped summariser"
        );
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
        // Re-activating a revision that does not validate now is recorded
        // as a failed revision of its own, naming the candidate and why,
        // and the one serving keeps serving.
        let refused_again = graph.activate(2, None).await.unwrap();
        assert_eq!(refused_again.revision, 3);
        assert_eq!(refused_again.activation, RevisionActivation::Failed);
        assert!(
            refused_again
                .failure
                .as_deref()
                .unwrap()
                .starts_with("activation of revision 2 refused: step 'curator'"),
            "{refused_again:?}"
        );
        assert_eq!(graph.active().revision, 0);
        let events = storage.list_audit_events().await.unwrap();
        assert!(
            events.iter().any(|event| event.entity_kind == "turn_graph"
                && event.entity_id == "3"
                && event.event_kind == "turn_graph_revision_failed"),
            "{events:?}"
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

        let mut proposed = with_succession(StepFiller::Workflow {
            name: "curate".into(),
        });
        proposed.hooks.push(HookAttachment {
            point: HookPoint::ToolCall,
            plugin: "guard".into(),
            mode: HookMode::Rewrite,
        });
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
        let active = graph.active();
        assert_eq!(active.graph.hooks_at(HookPoint::ToolCall).len(), 1);
        assert_eq!(active.graph.branches_from("succession").len(), 1);
        let diff = graph.diff(0, 1).unwrap();
        assert_eq!(diff.hooks_attached.len(), 1);
        assert_eq!(diff.branches_added.len(), 1);
        assert!(!diff.hooks_reordered);

        // A filler at a slot whose kernel point takes none is refused, not
        // recorded as serving: nothing would run it.
        let mut inert = TurnGraph::shipped();
        inert
            .steps
            .iter_mut()
            .find(|s| s.slot == TurnSlot::Curator)
            .unwrap()
            .filler = StepFiller::Workflow {
            name: "curate".into(),
        };
        let refused = graph.propose(inert, None).await.unwrap();
        assert_eq!(refused.activation, RevisionActivation::Failed);
        assert!(
            refused
                .failure
                .as_deref()
                .unwrap()
                .contains("slot 'curator' takes the kernel's own only"),
            "{refused:?}"
        );
        let mut composed = TurnGraph::shipped();
        composed
            .steps
            .iter_mut()
            .find(|s| s.slot == TurnSlot::ContextProgram)
            .unwrap()
            .filler = StepFiller::Workflow {
            name: "curate".into(),
        };
        let refused = graph.propose(composed, None).await.unwrap();
        assert!(
            refused
                .failure
                .as_deref()
                .unwrap()
                .contains("runs no workflow filler"),
            "{refused:?}"
        );

        // One plugin attached twice at a point, and a branch from a step the
        // kernel runs no shadow of, are refused by name.
        let mut twice = TurnGraph::shipped();
        for _ in 0..2 {
            twice.hooks.push(HookAttachment {
                point: HookPoint::ToolCall,
                plugin: "guard".into(),
                mode: HookMode::Observe,
            });
        }
        let refused = graph.propose(twice, None).await.unwrap();
        assert!(
            refused
                .failure
                .as_deref()
                .unwrap()
                .contains("attached there twice"),
            "{refused:?}"
        );
        let mut unshadowed = TurnGraph::shipped();
        unshadowed.branches.push(TurnBranch {
            id: "trial".into(),
            from: "curator".into(),
            steps: vec![step(
                "trial-curator",
                TurnSlot::Curator,
                StepFiller::Builtin,
            )],
        });
        let refused = graph.propose(unshadowed, None).await.unwrap();
        assert!(
            refused
                .failure
                .as_deref()
                .unwrap()
                .contains("runs no shadow of slot 'curator'"),
            "{refused:?}"
        );
        let mut mismatched = TurnGraph::shipped();
        mismatched.branches.push(TurnBranch {
            id: "trial".into(),
            from: "succession".into(),
            steps: vec![step(
                "trial-curator",
                TurnSlot::Curator,
                StepFiller::Builtin,
            )],
        });
        let refused = graph.propose(mismatched, None).await.unwrap();
        assert!(
            refused
                .failure
                .as_deref()
                .unwrap()
                .contains("with a step of slot 'curator', not 'succession'"),
            "{refused:?}"
        );

        // A revision whose recorded graph no longer reads is an error by
        // revision, never an empty graph.
        storage
            .record_turn_graph_revision(NewTurnGraphRevision {
                revision: 99,
                graph: serde_json::json!({ "steps": [{ "id": "x", "slot": "gone", "filler": { "kind": "builtin" } }] }),
                activation: "failed".into(),
                failure: None,
            })
            .await
            .unwrap();
        graph.load().await.unwrap();
        assert!(matches!(
            graph.revision(99),
            Err(TurnGraphError::Unreadable { revision: 99, .. })
        ));
        assert!(matches!(
            graph.list(),
            Err(TurnGraphError::Unreadable { revision: 99, .. })
        ));

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
        let unknown_workflow = with_succession(StepFiller::Workflow {
            name: "missing".into(),
        });
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

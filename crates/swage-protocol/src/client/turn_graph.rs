//! The turn graph: the versioned declaration of the turn pipeline's steps
//! and their order, which the gateway's kernel executes. A revision is
//! validated before it activates, inspectable, diffable and recorded; a
//! revision that fails validation is recorded as failed and the last good
//! one keeps serving.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// A named step of the turn the kernel runs: what a plugin fills or
/// replaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TurnSlot {
    /// Assembles the model's context from the turn's elements.
    ContextProgram,
    /// Summarises history into a successor when the context is over the
    /// threshold. The successor a thread takes after running into a wall is
    /// diagnosed by the provider's readers, not summarised, and stays the
    /// kernel's.
    Succession,
    /// Runs the model's tool calls through the session's gates.
    ToolDispatch,
    /// Writes memory pages from what the conversation said.
    Curator,
    /// Judges a request the session's grants do not cover.
    Judge,
}

impl TurnSlot {
    pub const ALL: [TurnSlot; 5] = [
        TurnSlot::ContextProgram,
        TurnSlot::Succession,
        TurnSlot::ToolDispatch,
        TurnSlot::Curator,
        TurnSlot::Judge,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            TurnSlot::ContextProgram => "context_program",
            TurnSlot::Succession => "succession",
            TurnSlot::ToolDispatch => "tool_dispatch",
            TurnSlot::Curator => "curator",
            TurnSlot::Judge => "judge",
        }
    }
}

/// What runs in a slot's place.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StepFiller {
    /// The kernel's own implementation.
    Builtin,
    /// An installed plugin that provides the slot.
    Plugin { name: String },
    /// A saved workflow script, run with the step's input as its arguments;
    /// its return value is the step's output.
    Workflow { name: String },
}

/// One step of the graph.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct TurnStep {
    /// The step's own name in the graph, unique across the live path and
    /// its branches.
    pub id: String,
    pub slot: TurnSlot,
    pub filler: StepFiller,
}

/// A point at which a hook sees a platform event before the kernel acts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum HookPoint {
    /// A message arriving for the session, before it is persisted.
    MessageInbound,
    /// A tool call the model raised, before the dispatcher's gates.
    ToolCall,
    TurnStart,
    TurnEnd,
}

impl HookPoint {
    pub fn as_str(self) -> &'static str {
        match self {
            HookPoint::MessageInbound => "message_inbound",
            HookPoint::ToolCall => "tool_call",
            HookPoint::TurnStart => "turn_start",
            HookPoint::TurnEnd => "turn_end",
        }
    }
}

/// What a hook may do with the event it sees.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum HookMode {
    /// Sees the event; cannot change it.
    Observe,
    /// May return a rewritten event, which the kernel then checks as if it
    /// had been raised fresh.
    Rewrite,
    /// May refuse the event with a reason.
    Veto,
}

/// A plugin's hook attached at a point.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct HookAttachment {
    pub point: HookPoint,
    pub plugin: String,
    pub mode: HookMode,
}

/// A sub-path that runs beside the live path from one of its steps — a
/// shadow: its outputs are recorded and compared, never served.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct TurnBranch {
    pub id: String,
    /// The live step this branch runs beside.
    pub from: String,
    pub steps: Vec<TurnStep>,
}

/// The declaration: the live path in order, the hooks attached, and the
/// branches that shadow it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct TurnGraph {
    pub steps: Vec<TurnStep>,
    #[serde(default)]
    #[ts(optional, as = "Option<_>")]
    pub hooks: Vec<HookAttachment>,
    #[serde(default)]
    #[ts(optional, as = "Option<_>")]
    pub branches: Vec<TurnBranch>,
}

/// The plugin the gateway ships for the succession step; installed by the
/// gateway's startup with provenance `shipped`.
pub const SHIPPED_SUCCESSION_SUMMARISER: &str = "succession-summariser";

impl TurnGraph {
    /// The graph the kernel ships with: the succession step filled by the
    /// shipped summariser plugin, every other slot the kernel's own, no
    /// hooks, no branches. Revision 0 on every install.
    pub fn shipped() -> Self {
        Self {
            steps: TurnSlot::ALL
                .iter()
                .map(|slot| TurnStep {
                    id: slot.as_str().to_owned(),
                    slot: *slot,
                    filler: match slot {
                        TurnSlot::Succession => StepFiller::Plugin {
                            name: SHIPPED_SUCCESSION_SUMMARISER.to_owned(),
                        },
                        _ => StepFiller::Builtin,
                    },
                })
                .collect(),
            hooks: Vec::new(),
            branches: Vec::new(),
        }
    }
}

impl TurnGraph {
    /// The live step carrying `slot`, when the graph has one.
    pub fn live_step(&self, slot: TurnSlot) -> Option<&TurnStep> {
        self.steps.iter().find(|step| step.slot == slot)
    }

    /// What serves `slot`: the kernel's own implementation unless the graph
    /// fills the slot otherwise.
    pub fn filler_for(&self, slot: TurnSlot) -> StepFiller {
        self.live_step(slot)
            .map(|step| step.filler.clone())
            .unwrap_or(StepFiller::Builtin)
    }

    /// The hooks attached at `point`, in order.
    pub fn hooks_at(&self, point: HookPoint) -> Vec<HookAttachment> {
        self.hooks
            .iter()
            .filter(|hook| hook.point == point)
            .cloned()
            .collect()
    }

    /// The branches shadowing the live step `step_id`.
    pub fn branches_from(&self, step_id: &str) -> Vec<TurnBranch> {
        self.branches
            .iter()
            .filter(|branch| branch.from == step_id)
            .cloned()
            .collect()
    }
}

/// Whether a recorded revision serves, failed to activate, or was replaced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RevisionActivation {
    Active,
    Failed,
    Superseded,
}

/// A recorded revision of the graph.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct TurnGraphRevision {
    pub revision: u32,
    pub graph: TurnGraph,
    pub activation: RevisionActivation,
    /// Why a failed revision was refused.
    pub failure: Option<String>,
    pub created_at: String,
}

/// The graph as it stands: the active revision and every revision recorded.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct TurnGraphResponse {
    pub active: u32,
    pub revisions: Vec<TurnGraphRevision>,
}

/// Propose a new revision: validated, recorded, and activated if valid.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct ProposeTurnGraphRequest {
    pub graph: TurnGraph,
}

/// A step whose filler changed between two revisions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct StepRefilled {
    pub id: String,
    pub before: StepFiller,
    pub after: StepFiller,
}

/// What changed from one revision to another.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct TurnGraphDiff {
    pub from: u32,
    pub to: u32,
    pub steps_added: Vec<TurnStep>,
    pub steps_removed: Vec<TurnStep>,
    pub steps_refilled: Vec<StepRefilled>,
    /// Steps present in both whose position in the live path changed.
    pub steps_reordered: Vec<String>,
    pub hooks_attached: Vec<HookAttachment>,
    pub hooks_detached: Vec<HookAttachment>,
    /// Whether the attachments present in both run in a different order —
    /// order is executable, since each hook at a point sees the event as
    /// the one before left it.
    pub hooks_reordered: bool,
    pub branches_added: Vec<TurnBranch>,
    pub branches_removed: Vec<TurnBranch>,
}

impl TurnGraphDiff {
    /// The structural difference between two revisions' graphs. A step is
    /// the same step across revisions when its id and its slot match: a
    /// step whose slot changed is one removed and one added, so no change
    /// to what a step runs can read as no change.
    pub fn between(from: u32, before: &TurnGraph, to: u32, after: &TurnGraph) -> Self {
        let before_steps: Vec<&TurnStep> = before.steps.iter().collect();
        let after_steps: Vec<&TurnStep> = after.steps.iter().collect();
        let find = |steps: &[&TurnStep], id: &str| {
            steps.iter().find(|step| step.id == id).copied().cloned()
        };
        let find = |steps: &[&TurnStep], probe: &TurnStep| {
            find(steps, &probe.id).filter(|found| found.slot == probe.slot)
        };
        let steps_added = after_steps
            .iter()
            .filter(|step| find(&before_steps, step).is_none())
            .map(|step| (*step).clone())
            .collect();
        let steps_removed = before_steps
            .iter()
            .filter(|step| find(&after_steps, step).is_none())
            .map(|step| (*step).clone())
            .collect();
        let steps_refilled = after_steps
            .iter()
            .filter_map(|step| {
                let earlier = find(&before_steps, step)?;
                (earlier.filler != step.filler).then(|| StepRefilled {
                    id: step.id.clone(),
                    before: earlier.filler,
                    after: step.filler.clone(),
                })
            })
            .collect();
        let shared_before: Vec<&str> = before_steps
            .iter()
            .filter(|step| find(&after_steps, step).is_some())
            .map(|step| step.id.as_str())
            .collect();
        let shared_after: Vec<&str> = after_steps
            .iter()
            .filter(|step| find(&before_steps, step).is_some())
            .map(|step| step.id.as_str())
            .collect();
        let steps_reordered = if shared_before == shared_after {
            Vec::new()
        } else {
            shared_after
                .iter()
                .zip(shared_before.iter())
                .filter(|(after, before)| after != before)
                .map(|(after, _)| (*after).to_owned())
                .collect()
        };
        let shared_hooks_before: Vec<&HookAttachment> = before
            .hooks
            .iter()
            .filter(|hook| after.hooks.contains(hook))
            .collect();
        let shared_hooks_after: Vec<&HookAttachment> = after
            .hooks
            .iter()
            .filter(|hook| before.hooks.contains(hook))
            .collect();
        Self {
            from,
            to,
            steps_added,
            steps_removed,
            steps_refilled,
            steps_reordered,
            hooks_reordered: shared_hooks_before != shared_hooks_after,
            hooks_attached: after
                .hooks
                .iter()
                .filter(|hook| !before.hooks.contains(hook))
                .cloned()
                .collect(),
            hooks_detached: before
                .hooks
                .iter()
                .filter(|hook| !after.hooks.contains(hook))
                .cloned()
                .collect(),
            branches_added: after
                .branches
                .iter()
                .filter(|branch| !before.branches.contains(branch))
                .cloned()
                .collect(),
            branches_removed: before
                .branches
                .iter()
                .filter(|branch| !after.branches.contains(branch))
                .cloned()
                .collect(),
        }
    }
}

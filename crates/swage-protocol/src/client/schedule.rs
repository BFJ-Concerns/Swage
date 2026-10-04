//! Schedules: durable definitions of recurring or one-off agent work.
//!
//! A schedule carries when to run (spec), what to ask (message), which
//! agent runs it and in what kind of session (session policy), where the
//! output goes (routing), and optionally a cheap-model gate that can decide
//! "no work needed" before the main model wakes.

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;
use uuid::Uuid;

use super::delivery::DeliveryRoutingRequest;

/// When a schedule fires.
///
/// - `one_shot` — a single RFC 3339 instant.
/// - `interval` — natural-language `"every N seconds|minutes|hours|days"`.
/// - `cron` — a cron expression (seconds-resolution, as parsed by the
///   `cron` crate).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[ts(optional_fields = nullable)]
pub enum ScheduleSpec {
    OneShot { at: String },
    Interval { every: String },
    Cron { cron: String },
}

/// Which session each run executes in.
///
/// - `fresh` — every run creates a new session in the selected topic, or
///   the general scope when `topic_id` is absent, for
///   `agent_id`, with a deliberately assembled opening context (the
///   schedule's message plus the previous run's outcome).
/// - `continuation` — every run continues the named session, executing as
///   whatever agent that session has at run time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, JsonSchema)]
#[serde(tag = "policy", rename_all = "snake_case")]
#[ts(optional_fields = nullable)]
pub enum SessionPolicy {
    Fresh {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional, as = "Option<_>")]
        topic_id: Option<Uuid>,
        agent_id: String,
    },
    Continuation {
        session_id: Uuid,
    },
}

/// Who created a schedule.
///
/// This is provenance, not a request field: the surface that creates the
/// schedule stamps it (the settings UI and HTTP surface stamp `user`; the
/// scheduling tools stamp `agent`), so an agent cannot claim a schedule was
/// a person's. It answers the audit question "where did this schedule come
/// from" wherever schedules are listed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[ts(optional_fields = nullable)]
pub enum ScheduleCreator {
    /// A person, through the settings automation panel or the HTTP surface.
    User,
    /// An agent, through the scheduling tools. Carries the id of the agent
    /// that made it.
    Agent { agent_id: String },
}

/// A cheap-model check that runs before the main model wakes.
///
/// The gate model inspects the schedule's message, the optional criteria,
/// and the previous run's outcome, then verdicts `run` or `skip`. Its
/// decision and reasoning are recorded on the run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct GateConfig {
    /// Model id or alias from the configured model catalogue.
    pub model: String,
    /// What "work needed" means for this schedule. When absent the gate
    /// judges from the schedule message and previous outcome alone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, as = "Option<_>")]
    pub criteria: Option<String>,
    /// State the gate is shown before it decides, beyond the schedule's own
    /// record: what the criteria name. Declared here so a criterion about a
    /// file is decidable against that file, and refused at declaration when
    /// the schedule's session has nowhere to read it from.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[ts(optional, as = "Option<_>")]
    pub observations: Vec<GateObservation>,
}

/// One thing a gate observes before deciding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[ts(optional_fields = nullable)]
pub enum GateObservation {
    /// A file under the scheduled session's trusted workspace, by a path
    /// relative to it: the gate sees whether it exists, its size, and its
    /// opening excerpt.
    File { path: String },
}

/// How an update touches a schedule's gate.
///
/// A gate is an optional field, and clearing an optional field over the
/// wire needs a value distinct from "leave unchanged". So the gate slot on
/// an update is itself optional: absent leaves the gate as it stands, while
/// a present value is one of these two actions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, JsonSchema)]
#[serde(tag = "action", rename_all = "snake_case")]
#[ts(optional_fields = nullable)]
pub enum ScheduleGateUpdate {
    /// Install this gate, replacing any existing one.
    Set(GateConfig),
    /// Remove the gate entirely.
    Clear,
}

/// Lifecycle status of a schedule definition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ScheduleStatus {
    Active,
    Paused,
    /// A one-shot that has fired, or an interval/cron with no next
    /// occurrence.
    Completed,
}

/// What caused a run to be created.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ScheduleRunTrigger {
    /// The scheduler's clock reached the schedule's next occurrence.
    Tick,
    /// An operator fired the schedule by hand.
    Manual,
}

/// Lifecycle state of a recorded run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ScheduleRunState {
    Pending,
    Running,
    Completed,
    /// Completed successfully but produced no assistant text.
    CompletedEmpty,
    Failed,
    /// Deliberately not executed — see the run's `skip_reason`.
    Skipped,
}

/// Why a run was skipped rather than executed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SkipReason {
    /// The gate model decided no work was needed.
    Gate,
    /// A previous run of this schedule (or another turn in the target
    /// session) was still executing when this run came due.
    Overlap,
}

/// The gate model's recorded verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum GateVerdict {
    Run,
    Skip,
}

/// A gate decision as recorded on a run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct GateDecision {
    pub model: String,
    pub verdict: GateVerdict,
    pub reasoning: String,
}

/// String round-trips for the enums stored as TEXT columns or shown in
/// UIs. The strings match the serde snake_case wire names, so a value
/// reads identically in the database, on the wire, and on screen.
macro_rules! string_round_trip {
    ($ty:ident { $($variant:ident => $text:literal),+ $(,)? }) => {
        impl std::fmt::Display for $ty {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                let s = match self {
                    $($ty::$variant => $text,)+
                };
                write!(f, "{s}")
            }
        }

        impl std::str::FromStr for $ty {
            type Err = String;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                match s {
                    $($text => Ok($ty::$variant),)+
                    other => Err(format!(
                        concat!("unknown ", stringify!($ty), ": {}"),
                        other
                    )),
                }
            }
        }
    };
}

string_round_trip!(ScheduleStatus {
    Active => "active",
    Paused => "paused",
    Completed => "completed",
});

string_round_trip!(ScheduleRunTrigger {
    Tick => "tick",
    Manual => "manual",
});

string_round_trip!(ScheduleRunState {
    Pending => "pending",
    Running => "running",
    Completed => "completed",
    CompletedEmpty => "completed_empty",
    Failed => "failed",
    Skipped => "skipped",
});

string_round_trip!(SkipReason {
    Gate => "gate",
    Overlap => "overlap",
});

string_round_trip!(GateVerdict {
    Run => "run",
    Skip => "skip",
});

/// Create a schedule.
#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct CreateScheduleRequest {
    /// What each run asks the agent to do.
    pub message: String,
    pub spec: ScheduleSpec,
    pub session: SessionPolicy,
    /// Defaults to local-record-only routing when omitted.
    #[serde(default)]
    #[ts(optional, as = "Option<_>")]
    pub routing: Option<DeliveryRoutingRequest>,
    #[serde(default)]
    #[ts(optional, as = "Option<_>")]
    pub gate: Option<GateConfig>,
}

/// Partially update a schedule.
///
/// The session policy is fixed at creation — delete and recreate to change
/// where runs execute.
#[derive(Debug, Clone, Default, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct UpdateScheduleRequest {
    #[serde(default)]
    #[ts(optional, as = "Option<_>")]
    pub message: Option<String>,
    #[serde(default)]
    #[ts(optional, as = "Option<_>")]
    pub spec: Option<ScheduleSpec>,
    #[serde(default)]
    #[ts(optional, as = "Option<_>")]
    pub routing: Option<DeliveryRoutingRequest>,
    /// Set or clear the gate. Absent leaves the existing gate untouched.
    #[serde(default)]
    #[ts(optional, as = "Option<_>")]
    pub gate: Option<ScheduleGateUpdate>,
}

/// A schedule as returned by the API.
#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct Schedule {
    pub id: Uuid,
    pub message: String,
    pub status: ScheduleStatus,
    pub spec: ScheduleSpec,
    pub session: SessionPolicy,
    /// Where this schedule came from — a person or an agent.
    pub creator: ScheduleCreator,
    pub routing: DeliveryRoutingRequest,
    pub gate: Option<GateConfig>,
    pub next_run_at: Option<DateTime<Utc>>,
    pub last_run_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// A recorded run: everything needed to reconstruct what happened.
#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct ScheduleRun {
    pub id: Uuid,
    pub schedule_id: Uuid,
    pub trigger: ScheduleRunTrigger,
    pub state: ScheduleRunState,
    pub skip_reason: Option<SkipReason>,
    pub scheduled_for: Option<DateTime<Utc>>,
    /// The session the run executed in — created fresh or continued,
    /// depending on the schedule's session policy. Absent when the run
    /// never reached a session (gated out, or skipped for overlap).
    pub session_id: Option<Uuid>,
    pub turn_id: Option<Uuid>,
    pub gate: Option<GateDecision>,
    pub output_text: Option<String>,
    pub error_message: Option<String>,
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct ScheduleListResponse {
    pub schedules: Vec<Schedule>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct ScheduleRunListResponse {
    pub runs: Vec<ScheduleRun>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct TriggerScheduleResponse {
    pub run: ScheduleRun,
}

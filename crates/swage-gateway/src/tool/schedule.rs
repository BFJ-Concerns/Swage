//! Schedule tools — the agent's hands on the gateway's schedule service.
//!
//! Two tools live here, both driving the same service the settings UI and the
//! HTTP surface use (`crate::schedule`), so an agent's standing work is a
//! first-class schedule, recorded and attributed like any other:
//!
//! - **`schedule`** — create, list, change, and remove standing schedules.
//! - **`wake_me_later`** — resume the current session once after a delay.
//!
//! Both need the live gateway state, so both run on the host
//! (`requires_device_execution`) and reach the state through the gateway tool
//! context. Every schedule they create is stamped with the calling agent's id
//! as its creator, so the audit trail can tell an agent's schedule from a
//! person's.

use async_trait::async_trait;
use chrono::{Duration, Utc};
use serde_json::{Value, json};
use uuid::Uuid;

use swage_core::{GatewayError, ToolError};
use swage_protocol::client::schedule as wire;
use swage_storage::ScheduleCreator;
use swage_tool::context::ToolContext;

use crate::state::AppState;
use crate::tool::Tool;
use crate::tool_executor::GatewayToolContext;

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// Reach the live gateway state through the host tool context. Both schedule
/// tools require it; a call that never reached the host path is a wiring bug,
/// reported as an execution failure rather than silently degrading.
fn gateway_state<'ctx>(
    tool: &str,
    ctx: &'ctx dyn ToolContext,
) -> Result<&'ctx AppState, ToolError> {
    ctx.as_any()
        .downcast_ref::<GatewayToolContext>()
        .and_then(GatewayToolContext::app_state)
        .ok_or_else(|| ToolError::ExecutionFailed {
            name: tool.to_owned(),
            message: "the schedule tools run only inside a gateway host turn".to_owned(),
        })
}

fn invalid(tool: &str, message: impl Into<String>) -> ToolError {
    ToolError::InvalidParameters {
        name: tool.to_owned(),
        message: message.into(),
    }
}

/// Map a schedule-service error onto the right tool error: a validation
/// failure is the model's to fix (bad spec, unknown space, empty message), so
/// it is `InvalidParameters`; anything else is an execution failure.
fn map_service_error(tool: &str, error: GatewayError) -> ToolError {
    match error {
        GatewayError::ValidationError(message) => invalid(tool, message),
        GatewayError::SpaceNotFound { .. } | GatewayError::ConversationNotFound { .. } => {
            invalid(tool, error.to_string())
        }
        other => ToolError::ExecutionFailed {
            name: tool.to_owned(),
            message: other.to_string(),
        },
    }
}

fn required_str<'p>(tool: &str, params: &'p Value, key: &str) -> Result<&'p str, ToolError> {
    params
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| invalid(tool, format!("'{key}' is required")))
}

fn parse_uuid(tool: &str, key: &str, value: &str) -> Result<Uuid, ToolError> {
    Uuid::parse_str(value.trim())
        .map_err(|_| invalid(tool, format!("'{key}' must be a UUID, got '{value}'")))
}

/// Parse a natural-language delay like `"30 minutes"` or `"2 hours"` into a
/// duration. Shares the vocabulary of interval schedules minus the `every`
/// prefix, since a wake is a single delay, not a recurrence.
fn parse_delay(tool: &str, text: &str) -> Result<Duration, ToolError> {
    let parts: Vec<&str> = text.split_whitespace().collect();
    let malformed = || {
        invalid(
            tool,
            "'delay' must look like 'N seconds|minutes|hours|days', e.g. two words",
        )
    };
    if parts.len() != 2 {
        return Err(malformed());
    }
    let quantity: i64 = parts[0].parse().map_err(|_| malformed())?;
    if quantity <= 0 {
        return Err(invalid(tool, "'delay' must be a positive amount of time"));
    }
    let seconds = match parts[1] {
        "second" | "seconds" => quantity,
        "minute" | "minutes" => quantity * 60,
        "hour" | "hours" => quantity * 60 * 60,
        "day" | "days" => quantity * 60 * 60 * 24,
        other => return Err(invalid(tool, format!("unknown time unit '{other}'"))),
    };
    Ok(Duration::seconds(seconds))
}

// ---------------------------------------------------------------------------
// ScheduleTool
// ---------------------------------------------------------------------------

pub struct ScheduleTool;

const SCHEDULE_TOOL: &str = "schedule";

#[async_trait]
impl Tool for ScheduleTool {
    fn name(&self) -> &str {
        SCHEDULE_TOOL
    }

    fn description(&self) -> &str {
        "Create and manage standing schedules — recurring or one-off background work the \
         gateway runs on its own clock, whether or not this conversation is still open. Set \
         `action` to create, list, update, or remove.\n\n\
         A schedule created here runs in its OWN fresh session by default, assembled with just \
         the task and the previous run's outcome — that is almost always what a recurring job \
         wants: a clean, minimal context each time, not the entire history of the conversation \
         that set it up. Do not reach for a running session to carry a repeating job; a schedule \
         is how repeating work stays isolated and cheap. Choose the `continuation` session only \
         when the job genuinely must build on one specific ongoing thread. To make THIS session \
         resume once after a wait, use `wake_me_later` instead — that is a self-continuation, not \
         a standing schedule.\n\n\
         Creating a schedule writes a durable record attributed to you (the calling agent) and \
         starts the clock immediately; each run is recorded in the schedule's run history. \
         Removing one is permanent. Cron expressions are evaluated in UTC."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["create", "list", "update", "remove"],
                    "description": "create: define a new schedule. list: return every schedule \
                                    with its status, next run time, and creator. update: change an \
                                    existing schedule's task, timing, or gate. remove: delete a \
                                    schedule permanently."
                },
                "schedule_id": {
                    "type": "string",
                    "description": "The schedule's UUID, from a create result or a list entry. \
                                    Required for update and remove; ignored otherwise."
                },
                "message": {
                    "type": "string",
                    "description": "The task each run asks the agent to do, written as a \
                                    self-contained brief — the run's session will not have this \
                                    conversation's context. Required for create; on update, \
                                    replaces the existing task."
                },
                "timing": {
                    "type": "string",
                    "enum": ["once", "every", "cron"],
                    "description": "How the schedule fires. once: a single moment (set `at`). \
                                    every: a fixed interval (set `every`). cron: a cron pattern \
                                    (set `cron`). Required for create; on update, changing it \
                                    re-arms the schedule from now."
                },
                "at": {
                    "type": "string",
                    "description": "For timing=once: the RFC 3339 instant to fire at, e.g. an \
                                    absolute UTC timestamp. Must be in the future."
                },
                "every": {
                    "type": "string",
                    "description": "For timing=every: a recurrence like 'N seconds', 'N minutes', \
                                    'N hours', or 'N days'. The first run happens one interval from \
                                    now."
                },
                "cron": {
                    "type": "string",
                    "description": "For timing=cron: a cron expression evaluated in UTC. Fires at \
                                    every matching instant."
                },
                "session": {
                    "type": "string",
                    "enum": ["fresh", "continuation"],
                    "description": "Which session each run executes in, fixed at creation. fresh: \
                                    a new session in `space_id` for `agent_id`, opened with only \
                                    the task and the last run's outcome — the isolated default for \
                                    recurring work. continuation: reuse the existing \
                                    `continuation_session_id`, running as whatever agent it \
                                    currently holds — pick this only when runs must build on one \
                                    specific thread. Required for create; cannot be changed after \
                                    (remove and recreate to move a schedule)."
                },
                "space_id": {
                    "type": "string",
                    "description": "For session=fresh: the UUID of the space each new run's \
                                    session is created in."
                },
                "agent_id": {
                    "type": "string",
                    "description": "For session=fresh: the id of the agent each new run executes \
                                    as."
                },
                "continuation_session_id": {
                    "type": "string",
                    "description": "For session=continuation: the UUID of the existing session \
                                    every run continues."
                },
                "gate_model": {
                    "type": "string",
                    "description": "Optional. A cheap model (a catalogue id or alias) that checks \
                                    before each clock-triggered run whether the work is actually \
                                    needed, skipping the run when nothing has changed. Omit for no \
                                    gate. On update, sets or replaces the gate; use \
                                    gate_action=clear to remove one."
                },
                "gate_criteria": {
                    "type": "string",
                    "description": "Optional, only meaningful with gate_model: a sentence telling \
                                    the gate what counts as work needed. Omitted, the gate judges \
                                    from the task and the previous outcome alone."
                },
                "gate_action": {
                    "type": "string",
                    "enum": ["set", "clear"],
                    "description": "For update only: set installs or replaces the gate from \
                                    gate_model/gate_criteria; clear removes any existing gate. \
                                    Omit to leave the gate unchanged."
                }
            },
            "required": ["action"]
        })
    }

    /// The schedule service lives in the gateway process; run there rather than
    /// in the sandbox child, which has no gateway state.
    fn requires_device_execution(&self) -> bool {
        true
    }

    async fn execute(&self, params: Value, ctx: &dyn ToolContext) -> Result<Value, ToolError> {
        let state = gateway_state(SCHEDULE_TOOL, ctx)?;
        let action = required_str(SCHEDULE_TOOL, &params, "action")?;
        match action {
            "create" => self.create(state, &params, ctx.agent_id()).await,
            "list" => self.list(state).await,
            "update" => self.update(state, &params).await,
            "remove" => self.remove(state, &params).await,
            other => Err(invalid(
                SCHEDULE_TOOL,
                format!("unknown action '{other}': use create, list, update, or remove"),
            )),
        }
    }
}

impl ScheduleTool {
    async fn create(
        &self,
        state: &AppState,
        params: &Value,
        agent_id: &str,
    ) -> Result<Value, ToolError> {
        let message = required_str(SCHEDULE_TOOL, params, "message")?.to_owned();
        let spec = build_spec(params)?;
        let session = build_session(params)?;
        let gate = build_gate(params)?;

        let request = wire::CreateScheduleRequest {
            message,
            spec,
            session,
            // Runs are recorded to the local run history; channel delivery is
            // not part of the agent-facing surface.
            routing: None,
            gate,
        };
        let creator = ScheduleCreator::Agent {
            agent_id: agent_id.to_owned(),
        };
        let schedule = crate::schedule::create_schedule(state, request, creator)
            .await
            .map_err(|error| map_service_error(SCHEDULE_TOOL, error))?;
        Ok(json!({ "schedule": schedule }))
    }

    async fn list(&self, state: &AppState) -> Result<Value, ToolError> {
        let response = crate::schedule::list_schedules(state)
            .await
            .map_err(|error| map_service_error(SCHEDULE_TOOL, error))?;
        Ok(json!({ "schedules": response.schedules }))
    }

    async fn update(&self, state: &AppState, params: &Value) -> Result<Value, ToolError> {
        let schedule_id = parse_uuid(
            SCHEDULE_TOOL,
            "schedule_id",
            required_str(SCHEDULE_TOOL, params, "schedule_id")?,
        )?;
        // A spec is optional on update — only rebuild one when a timing was given.
        let spec = if params.get("timing").is_some() {
            Some(build_spec(params)?)
        } else {
            None
        };
        let message = params
            .get("message")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(str::to_owned);
        let gate = build_gate_update(params)?;

        let request = wire::UpdateScheduleRequest {
            message,
            spec,
            routing: None,
            gate,
        };
        let schedule = crate::schedule::update_schedule(state, schedule_id, request)
            .await
            .map_err(|error| map_service_error(SCHEDULE_TOOL, error))?;
        Ok(json!({ "schedule": schedule }))
    }

    async fn remove(&self, state: &AppState, params: &Value) -> Result<Value, ToolError> {
        let schedule_id = parse_uuid(
            SCHEDULE_TOOL,
            "schedule_id",
            required_str(SCHEDULE_TOOL, params, "schedule_id")?,
        )?;
        let removed = crate::schedule::delete_schedule(state, schedule_id)
            .await
            .map_err(|error| map_service_error(SCHEDULE_TOOL, error))?;
        if !removed {
            return Err(invalid(
                SCHEDULE_TOOL,
                format!("no schedule with id {schedule_id}"),
            ));
        }
        Ok(json!({ "removed": true, "schedule_id": schedule_id }))
    }
}

/// Assemble a schedule spec from the flat timing parameters, checking that the
/// value matching the chosen `timing` is present.
fn build_spec(params: &Value) -> Result<wire::ScheduleSpec, ToolError> {
    let timing = required_str(SCHEDULE_TOOL, params, "timing")?;
    match timing {
        "once" => Ok(wire::ScheduleSpec::OneShot {
            at: required_str(SCHEDULE_TOOL, params, "at")?.to_owned(),
        }),
        "every" => Ok(wire::ScheduleSpec::Interval {
            every: required_str(SCHEDULE_TOOL, params, "every")?.to_owned(),
        }),
        "cron" => Ok(wire::ScheduleSpec::Cron {
            cron: required_str(SCHEDULE_TOOL, params, "cron")?.to_owned(),
        }),
        other => Err(invalid(
            SCHEDULE_TOOL,
            format!("unknown timing '{other}': use once, every, or cron"),
        )),
    }
}

/// Assemble a session policy from the flat session parameters.
fn build_session(params: &Value) -> Result<wire::SessionPolicy, ToolError> {
    let session = required_str(SCHEDULE_TOOL, params, "session")?;
    match session {
        "fresh" => Ok(wire::SessionPolicy::Fresh {
            space_id: parse_uuid(
                SCHEDULE_TOOL,
                "space_id",
                required_str(SCHEDULE_TOOL, params, "space_id")?,
            )?,
            agent_id: required_str(SCHEDULE_TOOL, params, "agent_id")?.to_owned(),
        }),
        "continuation" => Ok(wire::SessionPolicy::Continuation {
            session_id: parse_uuid(
                SCHEDULE_TOOL,
                "continuation_session_id",
                required_str(SCHEDULE_TOOL, params, "continuation_session_id")?,
            )?,
        }),
        other => Err(invalid(
            SCHEDULE_TOOL,
            format!("unknown session '{other}': use fresh or continuation"),
        )),
    }
}

/// Build the optional gate for creation: present only when a gate model was given.
fn build_gate(params: &Value) -> Result<Option<wire::GateConfig>, ToolError> {
    match params.get("gate_model").and_then(Value::as_str) {
        Some(model) if !model.trim().is_empty() => Ok(Some(wire::GateConfig {
            model: model.to_owned(),
            criteria: params
                .get("gate_criteria")
                .and_then(Value::as_str)
                .filter(|value| !value.trim().is_empty())
                .map(str::to_owned),
        })),
        _ => Ok(None),
    }
}

/// Build the gate change for an update from `gate_action`: absent leaves the
/// gate untouched, `set` installs one from gate_model/gate_criteria, `clear`
/// removes it.
fn build_gate_update(params: &Value) -> Result<Option<wire::ScheduleGateUpdate>, ToolError> {
    match params.get("gate_action").and_then(Value::as_str) {
        None => Ok(None),
        Some("clear") => Ok(Some(wire::ScheduleGateUpdate::Clear)),
        Some("set") => {
            let gate = build_gate(params)?.ok_or_else(|| {
                invalid(
                    SCHEDULE_TOOL,
                    "gate_action=set needs a gate_model to install",
                )
            })?;
            Ok(Some(wire::ScheduleGateUpdate::Set(gate)))
        }
        Some(other) => Err(invalid(
            SCHEDULE_TOOL,
            format!("unknown gate_action '{other}': use set or clear"),
        )),
    }
}

// ---------------------------------------------------------------------------
// WakeMeLaterTool
// ---------------------------------------------------------------------------

pub struct WakeMeLaterTool;

const WAKE_TOOL: &str = "wake_me_later";

#[async_trait]
impl Tool for WakeMeLaterTool {
    fn name(&self) -> &str {
        WAKE_TOOL
    }

    fn description(&self) -> &str {
        "Pause and resume THIS session once, after a delay. The conversation continues from \
         where it is now, with your `message` delivered as the next turn when the wait elapses — \
         a note to your future self for when there is nothing to do until then (waiting out a \
         cooldown, checking back after a build, following up later).\n\n\
         This is a one-off self-continuation, not a standing job: it fires once, in this same \
         session, keeping the current context. For work that should repeat, or that belongs in \
         its own clean session rather than piled onto this conversation, use `schedule` instead. \
         Reaching for this to run a recurring background task is the wrong tool — it would keep \
         appending to this ever-growing session.\n\n\
         Setting a wake starts the clock immediately and records a one-off schedule attributed \
         to you."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "delay": {
                    "type": "string",
                    "description": "How long to wait before resuming, as an amount and a unit: \
                                    'N seconds', 'N minutes', 'N hours', or 'N days'. Measured from \
                                    now."
                },
                "message": {
                    "type": "string",
                    "description": "The turn delivered to this session when the wait elapses — \
                                    what you want to be prompted with on waking, e.g. the thing to \
                                    check or continue."
                }
            },
            "required": ["delay", "message"]
        })
    }

    /// Needs the live gateway state and the current session id, so run on the host.
    fn requires_device_execution(&self) -> bool {
        true
    }

    async fn execute(&self, params: Value, ctx: &dyn ToolContext) -> Result<Value, ToolError> {
        let state = gateway_state(WAKE_TOOL, ctx)?;
        let delay = parse_delay(WAKE_TOOL, required_str(WAKE_TOOL, &params, "delay")?)?;
        let message = required_str(WAKE_TOOL, &params, "message")?.to_owned();
        let session_id = parse_uuid(WAKE_TOOL, "session", ctx.session_id())?;

        let wake_at = Utc::now() + delay;
        // A self-wake is a one-shot that continues this very session — the one
        // case where reusing the current conversation is exactly right.
        let request = wire::CreateScheduleRequest {
            message,
            spec: wire::ScheduleSpec::OneShot {
                at: wake_at.to_rfc3339(),
            },
            session: wire::SessionPolicy::Continuation { session_id },
            routing: None,
            gate: None,
        };
        let creator = ScheduleCreator::Agent {
            agent_id: ctx.agent_id().to_owned(),
        };
        let schedule = crate::schedule::create_schedule(state, request, creator)
            .await
            .map_err(|error| map_service_error(WAKE_TOOL, error))?;
        Ok(json!({ "schedule_id": schedule.id, "wake_at": wake_at.to_rfc3339() }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_delay_accepts_units() {
        assert_eq!(
            parse_delay(WAKE_TOOL, "30 minutes").unwrap(),
            Duration::minutes(30)
        );
        assert_eq!(
            parse_delay(WAKE_TOOL, "2 hours").unwrap(),
            Duration::hours(2)
        );
        assert_eq!(parse_delay(WAKE_TOOL, "1 day").unwrap(), Duration::days(1));
    }

    #[test]
    fn parse_delay_rejects_malformed() {
        assert!(parse_delay(WAKE_TOOL, "soon").is_err());
        assert!(parse_delay(WAKE_TOOL, "-5 minutes").is_err());
        assert!(parse_delay(WAKE_TOOL, "5 fortnights").is_err());
        assert!(parse_delay(WAKE_TOOL, "every 5 minutes").is_err());
    }

    #[test]
    fn build_spec_requires_matching_field() {
        let params = json!({"timing": "once", "at": "2027-01-01T00:00:00Z"});
        assert!(matches!(
            build_spec(&params).unwrap(),
            wire::ScheduleSpec::OneShot { .. }
        ));
        // timing=every without `every` is a validation error.
        assert!(build_spec(&json!({"timing": "every"})).is_err());
    }

    #[test]
    fn build_gate_update_reads_action() {
        assert!(build_gate_update(&json!({})).unwrap().is_none());
        assert!(matches!(
            build_gate_update(&json!({"gate_action": "clear"})).unwrap(),
            Some(wire::ScheduleGateUpdate::Clear)
        ));
        // set without a model is rejected.
        assert!(build_gate_update(&json!({"gate_action": "set"})).is_err());
        assert!(matches!(
            build_gate_update(&json!({"gate_action": "set", "gate_model": "small"})).unwrap(),
            Some(wire::ScheduleGateUpdate::Set(_))
        ));
    }
}

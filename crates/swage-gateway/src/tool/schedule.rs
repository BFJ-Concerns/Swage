//! Schedule tools — the agent's hands on the gateway's schedule service.
//!
//! Two tools live here, both driving the same service the settings UI and the
//! HTTP surface use (`crate::schedule`), so an agent's standing work is a
//! first-class schedule, recorded and attributed like any other:
//!
//! - **`schedule`** — create, list, change, pause, resume, and remove standing schedules.
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
use swage_protocol::client::delivery::DeliveryRoutingRequest;
use swage_protocol::client::schedule as wire;
use swage_storage::{ScheduleCreator, Storage, TopicStore};
use swage_tool::context::ToolContext;

use crate::state::AppState;
use crate::tool::Tool;

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
    super::require_gateway_state(
        ctx,
        tool,
        "the schedule tools run only inside a gateway host turn",
    )
}

fn invalid(tool: &str, message: impl Into<String>) -> ToolError {
    ToolError::InvalidParameters {
        name: tool.to_owned(),
        message: message.into(),
    }
}

/// Map a schedule-service error onto the right tool error: a validation
/// failure is the model's to fix (bad spec, unknown topic, empty message), so
/// it is `InvalidParameters`; anything else is an execution failure.
fn map_service_error(tool: &str, error: GatewayError) -> ToolError {
    match error {
        GatewayError::ValidationError(message) => invalid(tool, message),
        GatewayError::TopicNotFound { .. } | GatewayError::ConversationNotFound { .. } => {
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

fn delivery_target_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "kind": {
                "type": "string",
                "enum": ["channel", "local_storage", "webhook"]
            },
            "integration": {
                "type": "string",
                "enum": ["dev_dm", "dev_thread", "telegram"]
            },
            "account": {"type": "string"},
            "conversation": {"type": "string"},
            "url": {"type": "string"}
        },
        "additionalProperties": false,
        "oneOf": [
            {
                "type": "object",
                "properties": {
                    "kind": {"type": "string", "const": "channel"},
                    "integration": {
                        "type": "string",
                        "enum": ["dev_dm", "dev_thread", "telegram"]
                    },
                    "account": {"type": "string"},
                    "conversation": {"type": "string"}
                },
                "required": ["kind", "integration", "account", "conversation"],
                "additionalProperties": false
            },
            {
                "type": "object",
                "properties": {
                    "kind": {"type": "string", "const": "local_storage"}
                },
                "required": ["kind"],
                "additionalProperties": false
            },
            {
                "type": "object",
                "properties": {
                    "kind": {"type": "string", "const": "webhook"},
                    "url": {"type": "string"}
                },
                "required": ["kind", "url"],
                "additionalProperties": false
            }
        ]
    })
}

fn nullable_delivery_target_schema() -> Value {
    let mut schema = delivery_target_schema();
    schema["type"] = json!(["object", "null"]);
    schema["oneOf"]
        .as_array_mut()
        .expect("delivery target variants are an array")
        .insert(0, json!({"type": "null"}));
    schema
}

#[async_trait]
impl Tool for ScheduleTool {
    fn name(&self) -> &str {
        SCHEDULE_TOOL
    }

    fn description(&self) -> &str {
        "Create and manage standing schedules — recurring or one-off background work the \
         gateway runs on its own clock, whether or not this conversation is still open. Set \
         `action` to create, list, update, pause, resume, or remove.\n\n\
         You choose where each run executes with `session` (required on create). Almost always \
         set it to `fresh`: every run gets its own new session, opened with just the task and the \
         previous run's outcome — a clean, minimal context each time, not the entire history of \
         the conversation that set it up. Do not reach for a running session to carry a repeating \
         job; a fresh schedule is how repeating work stays isolated and cheap. Set `session` to \
         `continuation` only when the job genuinely must build on one specific ongoing thread. To \
         make THIS session resume once after a wait, use `wake_me_later` instead — that is a \
         self-continuation, not a standing schedule.\n\n\
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
                    "enum": ["create", "list", "update", "pause", "resume", "remove"],
                    "description": "create: define a new schedule. list: return every schedule \
                                    with its status, next run time, and creator. update: change an \
                                    existing schedule's task, timing, or gate (a gate may name files under the session's workspace to look at before deciding). pause: stop future \
                                    runs without deleting the schedule. resume: re-arm a paused \
                                    schedule from its stored timing. remove: delete a schedule \
                                    permanently."
                },
                "schedule_id": {
                    "type": "string",
                    "description": "The schedule's UUID, from a create result or a list entry. \
                                    Required for update, pause, resume, and remove; ignored \
                                    otherwise."
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
                    "description": "For timing=once: the RFC 3339 instant to fire at, as an \
                                    absolute UTC timestamp. A future instant fires then; an instant \
                                    already in the past is accepted and runs at the next tick — set \
                                    it in the past deliberately to run once, right away."
                },
                "every": {
                    "type": "string",
                    "description": "For timing=every: a recurrence of the form 'every N seconds', \
                                    'every N minutes', 'every N hours', or 'every N days' (the \
                                    literal word 'every' is required). The first run happens one \
                                    interval from now."
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
                                    a new session in the selected topic, or the general scope, \
                                    for `agent_id`, opened with only \
                                    the task and the last run's outcome — the isolated, recommended \
                                    choice for recurring work. continuation: reuse the existing \
                                    `continuation_session_id`, running as whatever agent it \
                                    currently holds — pick this only when runs must build on one \
                                    specific thread. Required for create (there is no default); \
                                    cannot be changed after (remove and recreate to move a \
                                    schedule)."
                },
                "topic_id": {
                    "type": ["string", "null"],
                    "description": "For session=fresh: the UUID of the topic each new run's \
                                    session is created in. Omit it to use this conversation's \
                                    scope; set null for the general scope."
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
                                    needed, skipping the run when nothing has changed. On create, \
                                    supplying it installs the gate; omit for no gate. On update it \
                                    takes effect only together with gate_action=set — gate_model \
                                    alone changes nothing."
                },
                "gate_criteria": {
                    "type": "string",
                    "description": "Optional, only meaningful with gate_model: a sentence telling \
                                    the gate what counts as work needed. Omitted, the gate judges \
                                    from the task and the previous outcome alone. A criterion \
                                    about a file or other workspace state needs that state \
                                    declared in gate_observe_files: without it the schedule is \
                                    created with a gate_warnings flag, and the gate skips such a \
                                    criterion rather than guess."
                },
                "gate_observe_files": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Optional, with gate_model: paths, relative to the session's \
                                    workspace, whose presence, size and opening lines the gate is \
                                    shown before it decides — what makes a criterion like 'only \
                                    when inbox/todo.txt is non-empty' decidable rather than \
                                    guessed. The schedule's session must have a trusted \
                                    workspace to read them from, or the gate is refused."
                },
                "gate_action": {
                    "type": "string",
                    "enum": ["set", "clear"],
                    "description": "For update only: set installs or replaces the gate from \
                                    gate_model/gate_criteria; clear removes any existing gate. \
                                    Omit to leave the gate unchanged."
                },
                "routing": {
                    "type": "object",
                    "description": "Where each completed run's output should go. Use channel for \
                                    a known conversation, local_storage to retain output locally, \
                                    or webhook to send it to an HTTPS endpoint. Schedules have no \
                                    originating channel, so deliver_to_origin defaults to false. \
                                    home_target names one primary destination; explicit_targets \
                                    is an object whose freely chosen labels name additional \
                                    destinations; store_local_output also retains a local copy. On \
                                    create, omit routing for the existing local-only default. On \
                                    update, omit it to leave delivery unchanged.",
                    "properties": {
                        "deliver_to_origin": {
                            "type": "boolean",
                            "description": "Whether to deliver back to the initiating channel. \
                                            Optional and defaults to false for schedules because \
                                            their runs have no channel origin."
                        },
                        "home_target": nullable_delivery_target_schema(),
                        "explicit_targets": {
                            "type": "object",
                            "description": "Additional destinations that should each receive the \
                                            completed output, keyed by any short unique label. The \
                                            labels identify entries only and are not persisted.",
                            "additionalProperties": delivery_target_schema()
                        },
                        "store_local_output": {
                            "type": "boolean",
                            "description": "Also retain each completed output in local storage."
                        }
                    },
                    "additionalProperties": false
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
        let session_id = parse_uuid(SCHEDULE_TOOL, "session", ctx.session_id())?;
        match action {
            "create" => {
                self.create(state, &params, ctx.agent_id(), session_id)
                    .await
            }
            "list" => self.list(state).await,
            "update" => self.update(state, &params, session_id).await,
            "pause" => self.pause(state, &params, session_id).await,
            "resume" => self.resume(state, &params, session_id).await,
            "remove" => self.remove(state, &params, session_id).await,
            other => Err(invalid(
                SCHEDULE_TOOL,
                format!(
                    "unknown action '{other}': use create, list, update, pause, resume, or remove"
                ),
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
        session_id: Uuid,
    ) -> Result<Value, ToolError> {
        let message = required_str(SCHEDULE_TOOL, params, "message")?.to_owned();
        let spec = build_spec(params)?;
        let session = build_session(
            state.storage.as_ref(),
            state.topic_store.as_ref(),
            params,
            session_id,
        )
        .await?;
        let gate = build_gate(params)?;
        let routing = build_routing(params)?;

        let request = wire::CreateScheduleRequest {
            message,
            spec,
            session,
            routing,
            gate,
        };
        let creator = ScheduleCreator::Agent {
            agent_id: agent_id.to_owned(),
        };
        let schedule = crate::schedule::create_schedule_change(
            state,
            request,
            creator,
            SCHEDULE_TOOL,
            Some(session_id),
        )
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

    async fn update(
        &self,
        state: &AppState,
        params: &Value,
        session_id: Uuid,
    ) -> Result<Value, ToolError> {
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
        let routing = build_routing(params)?;

        let request = wire::UpdateScheduleRequest {
            message,
            spec,
            routing,
            gate,
        };
        let schedule = crate::schedule::update_schedule_change(
            state,
            schedule_id,
            request,
            SCHEDULE_TOOL,
            Some(session_id),
        )
        .await
        .map_err(|error| map_service_error(SCHEDULE_TOOL, error))?;
        Ok(json!({ "schedule": schedule }))
    }

    async fn remove(
        &self,
        state: &AppState,
        params: &Value,
        session_id: Uuid,
    ) -> Result<Value, ToolError> {
        let schedule_id = schedule_id(params)?;
        let removed = crate::schedule::delete_schedule_change(
            state,
            schedule_id,
            SCHEDULE_TOOL,
            Some(session_id),
        )
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

    async fn pause(
        &self,
        state: &AppState,
        params: &Value,
        session_id: Uuid,
    ) -> Result<Value, ToolError> {
        let schedule_id = schedule_id(params)?;
        let schedule = crate::schedule::pause_schedule_change(
            state,
            schedule_id,
            SCHEDULE_TOOL,
            Some(session_id),
        )
        .await
        .map_err(|error| map_service_error(SCHEDULE_TOOL, error))?;
        Ok(json!({ "schedule": schedule }))
    }

    async fn resume(
        &self,
        state: &AppState,
        params: &Value,
        session_id: Uuid,
    ) -> Result<Value, ToolError> {
        let schedule_id = schedule_id(params)?;
        let schedule = crate::schedule::resume_schedule_change(
            state,
            schedule_id,
            SCHEDULE_TOOL,
            Some(session_id),
        )
        .await
        .map_err(|error| map_service_error(SCHEDULE_TOOL, error))?;
        Ok(json!({ "schedule": schedule }))
    }
}

fn schedule_id(params: &Value) -> Result<Uuid, ToolError> {
    parse_uuid(
        SCHEDULE_TOOL,
        "schedule_id",
        required_str(SCHEDULE_TOOL, params, "schedule_id")?,
    )
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
async fn build_session(
    storage: &dyn Storage,
    topic_store: &dyn TopicStore,
    params: &Value,
    calling_session_id: Uuid,
) -> Result<wire::SessionPolicy, ToolError> {
    let session = required_str(SCHEDULE_TOOL, params, "session")?;
    match session {
        "fresh" => {
            let explicit_topic_id = match params.get("topic_id") {
                None => None,
                Some(Value::Null) => return Ok(wire::SessionPolicy::Fresh {
                    topic_id: None,
                    agent_id: required_str(SCHEDULE_TOOL, params, "agent_id")?.to_owned(),
                }),
                Some(Value::String(value)) if value.trim().is_empty() => None,
                Some(Value::String(value)) => Some(parse_uuid(SCHEDULE_TOOL, "topic_id", value)
                    .map_err(|_| {
                        invalid(
                            SCHEDULE_TOOL,
                            format!(
                                "'topic_id' must be a UUID when provided, got '{value}'; omit it to use this conversation's topic"
                            ),
                        )
                    })?),
                Some(_) => {
                    return Err(invalid(
                        SCHEDULE_TOOL,
                        "'topic_id' must be a UUID string when provided; omit it to use this conversation's topic",
                    ));
                }
            };
            let topic_id = match explicit_topic_id {
                Some(topic_id) => topic_id,
                None => {
                    storage
                        .get_session(calling_session_id)
                        .await
                        .map_err(|error| ToolError::ExecutionFailed {
                            name: SCHEDULE_TOOL.to_owned(),
                            message: format!("failed to resolve the calling session: {error}"),
                        })?
                        .ok_or_else(|| {
                            invalid(
                                SCHEDULE_TOOL,
                                format!("calling session '{calling_session_id}' was not found"),
                            )
                        })?
                        .topic_id
                }
            };
            let general_id = topic_store
                .general_scope()
                .await
                .map_err(|error| ToolError::ExecutionFailed {
                    name: SCHEDULE_TOOL.to_owned(),
                    message: format!("failed to resolve the general scope: {error}"),
                })?
                .id;
            Ok(wire::SessionPolicy::Fresh {
                topic_id: (topic_id != general_id).then_some(topic_id),
                agent_id: required_str(SCHEDULE_TOOL, params, "agent_id")?.to_owned(),
            })
        }
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
            observations: match params.get("gate_observe_files") {
                None | Some(Value::Null) => Vec::new(),
                Some(Value::Array(paths)) => paths
                    .iter()
                    .map(|path| {
                        path.as_str()
                            .map(|path| wire::GateObservation::File {
                                path: path.trim().to_owned(),
                            })
                            .ok_or_else(|| {
                                invalid(SCHEDULE_TOOL, "gate_observe_files holds paths as strings")
                            })
                    })
                    .collect::<Result<Vec<_>, _>>()?,
                Some(_) => {
                    return Err(invalid(
                        SCHEDULE_TOOL,
                        "gate_observe_files is an array of workspace-relative paths",
                    ));
                }
            },
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

/// Parse the shared delivery-routing wire shape when supplied. Absence has
/// action-specific meaning in the schedule service: create uses its local-only
/// default, while update preserves the existing route.
fn build_routing(params: &Value) -> Result<Option<DeliveryRoutingRequest>, ToolError> {
    params
        .get("routing")
        .cloned()
        .map(|mut routing| {
            let object = routing.as_object_mut().ok_or_else(|| {
                invalid(SCHEDULE_TOOL, "'routing' must be a delivery route object")
            })?;
            object
                .entry("deliver_to_origin")
                .or_insert(Value::Bool(false));
            if let Some(explicit_targets) = object.get_mut("explicit_targets") {
                let targets = explicit_targets
                    .as_object()
                    .ok_or_else(|| {
                        invalid(
                            SCHEDULE_TOOL,
                            "'routing.explicit_targets' must be an object keyed by label",
                        )
                    })?
                    .values()
                    .cloned()
                    .collect();
                *explicit_targets = Value::Array(targets);
            }
            serde_json::from_value(routing).map_err(|error| {
                invalid(
                    SCHEDULE_TOOL,
                    format!("'routing' must be a valid delivery route: {error}"),
                )
            })
        })
        .transpose()
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
        let schedule = crate::schedule::create_schedule_change(
            state,
            request,
            creator,
            WAKE_TOOL,
            Some(session_id),
        )
        .await
        .map_err(|error| map_service_error(WAKE_TOOL, error))?;
        Ok(json!({
            "schedule_id": schedule.id,
            "wake_at": wake_at.to_rfc3339(),
        }))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use swage_storage::{NewSession, SessionStore, SqliteStorage, TopicStore};

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

    #[tokio::test(flavor = "multi_thread")]
    async fn fresh_session_defaults_to_the_calling_sessions_topic() {
        let storage = Arc::new(SqliteStorage::in_memory().unwrap());
        let topic = storage.general_scope().await.unwrap();
        let session = storage
            .create_session(NewSession {
                agent_id: "assistant".into(),
                ..swage_storage::fixtures::new_session(topic.id)
            })
            .await
            .unwrap();

        let policy = build_session(
            storage.as_ref(),
            storage.as_ref(),
            &json!({"session": "fresh", "agent_id": "assistant"}),
            session.id,
        )
        .await
        .unwrap();

        assert_eq!(
            policy,
            wire::SessionPolicy::Fresh {
                topic_id: None,
                agent_id: "assistant".into(),
            }
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn fresh_session_preserves_an_explicit_topic_without_resolving_the_caller() {
        let storage = Arc::new(SqliteStorage::in_memory().unwrap());
        let explicit_topic_id = Uuid::new_v4();

        let policy = build_session(
            storage.as_ref(),
            storage.as_ref(),
            &json!({
                "session": "fresh",
                "topic_id": explicit_topic_id,
                "agent_id": "assistant"
            }),
            Uuid::new_v4(),
        )
        .await
        .unwrap();

        assert!(matches!(
            policy,
            wire::SessionPolicy::Fresh { topic_id, .. } if topic_id == Some(explicit_topic_id)
        ));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn fresh_session_rejects_a_missing_calling_session_when_topic_is_omitted() {
        let storage = Arc::new(SqliteStorage::in_memory().unwrap());

        let error = build_session(
            storage.as_ref(),
            storage.as_ref(),
            &json!({"session": "fresh", "agent_id": "assistant"}),
            Uuid::new_v4(),
        )
        .await
        .unwrap_err();

        assert!(matches!(error, ToolError::InvalidParameters { .. }));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn fresh_session_rejects_an_invalid_explicit_topic() {
        let storage = Arc::new(SqliteStorage::in_memory().unwrap());

        let error = build_session(
            storage.as_ref(),
            storage.as_ref(),
            &json!({
                "session": "fresh",
                "topic_id": "not-a-uuid",
                "agent_id": "assistant"
            }),
            Uuid::new_v4(),
        )
        .await
        .unwrap_err();

        assert!(matches!(
            error,
            ToolError::InvalidParameters { message, .. }
                if message.contains("omit it to use this conversation's topic")
        ));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn fresh_session_null_targets_general_and_blank_inherits_the_calling_topic() {
        let storage = Arc::new(SqliteStorage::in_memory().unwrap());
        let topic = storage
            .create(swage_storage::NewTopic {
                name: "Calling topic".into(),
                slug: "calling-topic".into(),
            })
            .await
            .unwrap();
        let session = storage
            .create_session(NewSession {
                agent_id: "assistant".into(),
                ..swage_storage::fixtures::new_session(topic.id)
            })
            .await
            .unwrap();

        for (topic_id, expected_topic_id) in [
            (Value::Null, None),
            (Value::String("  ".into()), Some(topic.id)),
        ] {
            let policy = build_session(
                storage.as_ref(),
                storage.as_ref(),
                &json!({
                    "session": "fresh",
                    "topic_id": topic_id,
                    "agent_id": "assistant"
                }),
                session.id,
            )
            .await
            .unwrap();

            assert!(matches!(
                policy,
                wire::SessionPolicy::Fresh { topic_id, .. } if topic_id == expected_topic_id
            ));
        }
    }
}

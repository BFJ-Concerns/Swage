//! The workflow engine's side of the isolate's door.
//!
//! Every host function the script shim offers (`agent`, `workflow`, the
//! timers, holds and events, the budget reads) arrives here as one named call
//! with its arguments as a JSON array, and leaves as the JSON envelope the
//! shim unwraps: `{"ok": true, "value": …}` or `{"ok": false, "error":
//! {"name", "message", …}}`. Capability is decided twice — the shim defines
//! only the globals the run was granted, and a call for anything else is
//! refused here — so a script that reaches past its shim finds a locked door,
//! not a stubbed one.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};
use swage_isolate::HostCalls;
use tokio_util::sync::CancellationToken;

use crate::engine::{ScriptCapabilities, is_pre_spawn_author_error, named_host_error};
use crate::{
    AgentCallRequest, AwaitEventRequest, EffectPayload, EventDelivery, GateRequest, HeldWorkerOpen,
    HoldRequest, RunEventPost, WorkflowCallRequest, WorkflowHost, WorkflowHostError,
};

pub(crate) struct WorkflowHostCalls {
    host: Arc<dyn WorkflowHost>,
    cancellation: CancellationToken,
    capabilities: ScriptCapabilities,
}

impl WorkflowHostCalls {
    pub(crate) fn new(
        host: Arc<dyn WorkflowHost>,
        cancellation: CancellationToken,
        capabilities: ScriptCapabilities,
    ) -> Self {
        Self {
            host,
            cancellation,
            capabilities,
        }
    }
}

fn ok(value: impl Into<Value>) -> String {
    json!({"ok": true, "value": value.into()}).to_string()
}

fn failure(name: &str, message: impl Into<String>) -> String {
    json!({"ok": false, "error": {"name": name, "message": message.into()}}).to_string()
}

fn host_failure(error: &str, fallback: &str) -> String {
    let (name, message) = named_host_error(error, fallback);
    failure(&name, message)
}

fn pre_spawn_failure(name: &str, message: impl Into<String>) -> String {
    json!({"ok": false, "error": {"name": name, "message": message.into(), "pre_spawn": true}})
        .to_string()
}

fn ungranted(name: &str) -> String {
    failure(
        "HostCapabilityError",
        format!("the host grants no call named '{name}' to this run"),
    )
}

fn arguments(payload: &str) -> Vec<Value> {
    serde_json::from_str(payload).unwrap_or_default()
}

fn string_argument(arguments: &[Value], index: usize) -> String {
    arguments
        .get(index)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_default()
}

fn number_argument(arguments: &[Value], index: usize) -> f64 {
    arguments
        .get(index)
        .and_then(Value::as_f64)
        .unwrap_or(f64::NAN)
}

fn parse_argument<T: serde::de::DeserializeOwned>(
    arguments: &[Value],
    index: usize,
    error_name: &str,
) -> Result<T, String> {
    serde_json::from_str(&string_argument(arguments, index))
        .map_err(|error| failure(error_name, error.to_string()))
}

fn worker_id(arguments: &[Value]) -> Result<uuid::Uuid, String> {
    string_argument(arguments, 0)
        .parse::<uuid::Uuid>()
        .map_err(|error| failure("HeldWorkerError", error.to_string()))
}

#[async_trait]
impl HostCalls for WorkflowHostCalls {
    async fn call(&self, name: &str, payload: String) -> String {
        let arguments = arguments(&payload);
        let cancellation = self.cancellation.clone();
        match name {
            "agent" => {
                let prompt = string_argument(&arguments, 0);
                let options = string_argument(&arguments, 1);
                self.agent(prompt, &options, cancellation).await
            }
            "workerPlacements" => match self.host.list_worker_placements(cancellation).await {
                Ok(value) => ok(value),
                Err(error) => json!({"ok": false, "error": error}).to_string(),
            },
            "capturedSources" if self.capabilities.captured_sources => {
                match self.host.captured_sources(cancellation).await {
                    Ok(value) => ok(value),
                    Err(error) => host_failure(&error, "KnowledgeStoreError"),
                }
            }
            "workflow" if self.capabilities.composition => {
                let name = string_argument(&arguments, 0);
                let args: Value = match serde_json::from_str(&string_argument(&arguments, 1)) {
                    Ok(args) => args,
                    Err(error) => {
                        return pre_spawn_failure("WorkflowArgumentsError", error.to_string());
                    }
                };
                match self
                    .host
                    .run_workflow_call(WorkflowCallRequest { name, args }, cancellation)
                    .await
                {
                    Ok(output) => ok(output),
                    Err(error) => {
                        let (name, message) = named_host_error(&error, "WorkflowCompositionError");
                        json!({
                            "ok": false,
                            "error": {"name": name, "message": message, "pre_spawn": is_pre_spawn_author_error(&error)}
                        })
                        .to_string()
                    }
                }
            }
            "workerOpen" => {
                let worker: HeldWorkerOpen = match parse_argument(&arguments, 0, "HeldWorkerError")
                {
                    Ok(worker) => worker,
                    Err(refusal) => return refusal,
                };
                match self.host.open_held_worker(worker, cancellation).await {
                    Ok(value) => ok(value),
                    Err(error) => host_failure(&error, "HeldWorkerError"),
                }
            }
            "workerClose" => {
                let worker_id = match worker_id(&arguments) {
                    Ok(worker_id) => worker_id,
                    Err(refusal) => return refusal,
                };
                match self.host.close_held_worker(worker_id, cancellation).await {
                    Ok(value) => ok(value),
                    Err(error) => host_failure(&error, "HeldWorkerError"),
                }
            }
            "workerStaleness" => {
                let worker_id = match worker_id(&arguments) {
                    Ok(worker_id) => worker_id,
                    Err(refusal) => return refusal,
                };
                match self
                    .host
                    .held_worker_staleness(worker_id, cancellation)
                    .await
                {
                    Ok(value) => ok(value),
                    Err(error) => host_failure(&error, "HeldWorkerError"),
                }
            }
            "timer" => {
                let ms = number_argument(&arguments, 0);
                let timer_id = number_argument(&arguments, 1);
                if !ms.is_finite() || ms < 0.0 {
                    return failure(
                        "WaitTimerError",
                        "timerMs must be a non-negative finite number",
                    );
                }
                match self
                    .host
                    .timer(ms as u64, timer_id as u64, cancellation)
                    .await
                {
                    Ok(value) => ok(value),
                    Err(error) => host_failure(&error, "WaitTimerError"),
                }
            }
            "confirmTimer" => {
                let timer_id = number_argument(&arguments, 0);
                match self.host.confirm_timer(timer_id as u64).await {
                    Ok(()) => ok(Value::Null),
                    Err(error) => host_failure(&error, "WaitTimerError"),
                }
            }
            "postEvent" => {
                let kind = string_argument(&arguments, 0);
                let payload: Value = match parse_argument(&arguments, 1, "RunEventError") {
                    Ok(payload) => payload,
                    Err(refusal) => return refusal,
                };
                match self
                    .host
                    .post_run_event(RunEventPost { kind, payload }, cancellation)
                    .await
                {
                    Ok(value) => ok(value),
                    Err(error) => host_failure(&error, "RunEventError"),
                }
            }
            "awaitEvent" => {
                let request: AwaitEventRequest =
                    match parse_argument(&arguments, 0, "RunEventError") {
                        Ok(request) => request,
                        Err(refusal) => return refusal,
                    };
                match self.host.await_run_event(request, cancellation).await {
                    Ok(value) => ok(value),
                    Err(error) => host_failure(&error, "RunEventError"),
                }
            }
            "hold" => {
                let request: HoldRequest = match parse_argument(&arguments, 0, "HoldError") {
                    Ok(request) => request,
                    Err(refusal) => return refusal,
                };
                match self.host.raise_hold(request, cancellation).await {
                    Ok(value) => ok(value),
                    Err(error) => host_failure(&error, "HoldError"),
                }
            }
            "gate" => {
                let request: GateRequest = match parse_argument(&arguments, 0, "RunEventError") {
                    Ok(request) => request,
                    Err(refusal) => return refusal,
                };
                match self.host.evaluate_wake_gate(request, cancellation).await {
                    Ok(value) => ok(value),
                    Err(error) => host_failure(&error, "RunEventError"),
                }
            }
            "deliverEvents" => {
                let selection: EventDelivery =
                    match serde_json::from_str(&string_argument(&arguments, 0)) {
                        Ok(selection) => selection,
                        Err(error) => {
                            return failure(
                                "RunEventError",
                                format!("invalid event selection: {error}"),
                            );
                        }
                    };
                match self.host.deliver_run_events(selection, cancellation).await {
                    Ok(value) => ok(value),
                    Err(error) => host_failure(&error, "RunEventError"),
                }
            }
            "deliver" if self.capabilities.delivery => {
                let payload: EffectPayload = match parse_argument(&arguments, 0, "DeliveryError") {
                    Ok(payload) => payload,
                    Err(refusal) => return refusal,
                };
                match self.host.deliver(payload, cancellation).await {
                    Ok(receipt) => ok(serde_json::to_value(receipt).unwrap_or(Value::Null)),
                    Err(error) => host_failure(&error, "DeliveryError"),
                }
            }
            "budgetSnapshot" => match self.host.budget_snapshot(cancellation).await {
                Ok(value) => ok(value),
                Err(error) => host_failure(&error, "BudgetSnapshotError"),
            },
            other => ungranted(other),
        }
    }

    fn call_sync(&self, name: &str, payload: String) -> String {
        let arguments = arguments(&payload);
        match name {
            "log" | "phase" => {
                self.host.record(name, string_argument(&arguments, 0));
                Value::Null.to_string()
            }
            "abandonTimer" => {
                self.host
                    .abandon_timer(number_argument(&arguments, 0) as u64);
                Value::Null.to_string()
            }
            "budgetSpent" => self.host.budget_spent().to_string(),
            // `null` reads as `undefined` in the shim: an unbounded budget has
            // no remaining figure.
            "budgetRemaining" => serde_json::to_string(&self.host.budget_remaining())
                .unwrap_or_else(|_| Value::Null.to_string()),
            other => ungranted(other),
        }
    }
}

impl WorkflowHostCalls {
    async fn agent(
        &self,
        prompt: String,
        options: &str,
        cancellation: CancellationToken,
    ) -> String {
        let mut request: AgentCallRequest = match serde_json::from_str(options) {
            Ok(request) => request,
            Err(error) => return pre_spawn_failure("InvalidAgentOptionError", error.to_string()),
        };
        request.prompt = prompt;
        match self
            .host
            .dispatch_agent_call(request.clone(), cancellation)
            .await
        {
            Ok(result) => {
                if request.schema.is_none()
                    && result
                        .output
                        .as_str()
                        .is_some_and(|value| value.trim().is_empty())
                {
                    return json!({
                        "ok": false,
                        "error": {
                            "name": "EmptyAgentOutputError",
                            "message": "agent returned empty output",
                            "agent_call_admitted": true,
                        }
                    })
                    .to_string();
                }
                // Worker metadata rides beside the output so `agentStart`
                // handles can expose it without touching agent()'s bare-value
                // contract.
                let session = result
                    .session_id
                    .zip(result.thread_id)
                    .map(|(session_id, thread_id)| {
                        json!({"session_id": session_id, "thread_id": thread_id})
                    });
                let value = if request.identity == Some(true) {
                    json!({
                        "label": request.label,
                        "phase": request.phase,
                        "output": result.output,
                        "failure": null,
                    })
                } else {
                    result.output
                };
                json!({
                    "ok": true,
                    "value": value,
                    "session": session,
                    "working_directory": result.working_directory,
                })
                .to_string()
            }
            Err(WorkflowHostError::SchemaExhausted { message, .. }) => {
                // Exhausting a schema's validation attempts is a
                // script-visible absence, not an ordinary agent failure.
                // Identified calls retain that distinction for a fan-in;
                // unmarked calls keep the bare-null contract.
                let value = if request.identity == Some(true) {
                    json!({
                        "label": request.label,
                        "phase": request.phase,
                        "output": null,
                        "failure": {"message": message},
                    })
                } else {
                    Value::Null
                };
                json!({"ok": true, "value": value, "session": null}).to_string()
            }
            Err(error) => {
                let (host_reference, error) = match error {
                    WorkflowHostError::AgentCall {
                        host_reference,
                        message,
                    } => (Some(host_reference), message),
                    WorkflowHostError::Message(message) => (None, message),
                    WorkflowHostError::SchemaExhausted { .. } => {
                        unreachable!("schema exhaustion is handled above")
                    }
                };
                let (name, message) = named_host_error(&error, "AgentCallError");
                json!({
                    "ok": false,
                    "error": {
                        "name": name,
                        "message": message,
                        "host_reference": host_reference,
                        "agent_call_admitted": true,
                        "pre_spawn": is_pre_spawn_author_error(&error),
                    }
                })
                .to_string()
            }
        }
    }
}

//! Gateway policy adapter for Codex App Server turns.

use std::collections::HashMap;
use std::{path::Path, sync::Arc, time::Instant};

use serde_json::{Value, json};
use swage_agent::{AgentConfig, AgentError, AgentEvent, OutboundMessage};
use swage_codex::{AppServerClient, ClientError, ContentItem, DynamicToolSpec, ToolCallResponse};
use swage_core::{ChatMessage, Role, StorageError};
use swage_storage::{
    model::{AppendMessage, NewAuditEvent, NewToolCall, ThreadState, TurnState},
    store::Storage,
};
use swage_tool::{
    dispatch::ToolDispatcher,
    permission::{PermissionCheckResult, PermissionConfig, check_permission},
};
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::state::AppState;
use swage_protocol::client::provider_signin::{
    ProviderAccount, ProviderSignInState, ProviderSignInStatus, SignInMethod,
    StartProviderSignInRequest, StartProviderSignInResponse,
};

pub async fn sign_in_state(state: &AppState) -> Result<ProviderSignInState, String> {
    let response = state
        .codex_supervisor
        .client()
        .await
        .map_err(|error| error.to_string())?
        .request("account/read", json!({"refreshToken":true}))
        .await;
    let response = match response {
        Ok(response) => response,
        Err(ClientError::Rpc { message, .. }) if is_authentication_error(&message) => {
            return Ok(ProviderSignInState {
                status: ProviderSignInStatus::Expired,
                account: None,
            });
        }
        Err(error) => return Err(error.to_string()),
    };
    let account = response.get("account").filter(|value| !value.is_null());
    Ok(match account {
        Some(account) => ProviderSignInState {
            status: ProviderSignInStatus::SignedIn,
            account: Some(ProviderAccount {
                email: account
                    .get("email")
                    .and_then(Value::as_str)
                    .map(ToString::to_string),
                plan: account
                    .get("planType")
                    .and_then(Value::as_str)
                    .map(ToString::to_string),
            }),
        },
        None => ProviderSignInState {
            status: ProviderSignInStatus::SignedOut,
            account: None,
        },
    })
}

fn is_authentication_error(message: &str) -> bool {
    let message = message.to_ascii_lowercase();
    message.contains("unauthorised")
        || message.contains("unauthorized")
        || message.contains("authentication")
        || message.contains("not logged in")
}

pub async fn start_sign_in(
    state: &AppState,
    request: StartProviderSignInRequest,
) -> Result<StartProviderSignInResponse, String> {
    let login_type = match request.method {
        SignInMethod::Browser => "chatgpt",
        SignInMethod::DeviceCode => "chatgptDeviceCode",
    };
    let response = state
        .codex_supervisor
        .client()
        .await
        .map_err(|error| error.to_string())?
        .request("account/login/start", json!({"type":login_type}))
        .await
        .map_err(|error| error.to_string())?;
    let login_id = response
        .get("loginId")
        .and_then(Value::as_str)
        .ok_or("sign-in response omitted loginId")?
        .to_string();
    match request.method {
        SignInMethod::Browser => Ok(StartProviderSignInResponse::Browser {
            auth_url: response
                .get("authUrl")
                .and_then(Value::as_str)
                .ok_or("sign-in response omitted authUrl")?
                .to_string(),
            login_id,
        }),
        SignInMethod::DeviceCode => Ok(StartProviderSignInResponse::DeviceCode {
            verification_url: response
                .get("verificationUrl")
                .and_then(Value::as_str)
                .ok_or("sign-in response omitted verificationUrl")?
                .to_string(),
            user_code: response
                .get("userCode")
                .and_then(Value::as_str)
                .ok_or("sign-in response omitted userCode")?
                .to_string(),
            login_id,
        }),
    }
}

pub async fn sign_out(state: &AppState) -> Result<ProviderSignInState, String> {
    state
        .codex_supervisor
        .client()
        .await
        .map_err(|error| error.to_string())?
        .request("account/logout", json!({}))
        .await
        .map_err(|error| error.to_string())?;
    Ok(ProviderSignInState {
        status: ProviderSignInStatus::SignedOut,
        account: None,
    })
}

#[derive(Clone)]
pub struct CodexTurnRequest {
    pub session_id: Uuid,
    pub thread_id: Uuid,
    pub turn_id: Uuid,
    pub agent_id: String,
    pub model: String,
    pub user_message: String,
    pub assembled_context: Vec<ChatMessage>,
    pub agent_config: AgentConfig,
    pub permissions: PermissionConfig,
    pub dispatcher: Arc<ToolDispatcher>,
    pub event_tx: mpsc::UnboundedSender<AgentEvent>,
    pub cancellation: CancellationToken,
}

pub async fn run_turn(
    state: AppState,
    request: CodexTurnRequest,
) -> Result<OutboundMessage, AgentError> {
    let turn_id = request.turn_id;
    let thread_id = request.thread_id;
    let event_tx = request.event_tx.clone();
    let result = run_turn_inner(state.clone(), request).await;
    if let Err(error) = &result
        && !matches!(error, AgentError::Cancelled)
    {
        if let Err(storage_error) = state
            .storage
            .update_turn_state(turn_id, TurnState::Failed)
            .await
        {
            tracing::warn!(%turn_id, error = %storage_error, "failed to mark Codex turn failed");
        }
        if let Err(storage_error) = state
            .storage
            .update_state(thread_id, ThreadState::Idle)
            .await
        {
            tracing::warn!(%thread_id, error = %storage_error, "failed to return Codex thread to idle");
        }
        let _ = event_tx.send(AgentEvent::Error {
            message: error.to_string(),
        });
    }
    result
}

async fn run_turn_inner(
    state: AppState,
    request: CodexTurnRequest,
) -> Result<OutboundMessage, AgentError> {
    let handle = state
        .codex_supervisor
        .ensure_running()
        .await
        .map_err(|error| {
            AgentError::ProviderFailed(swage_core::ProviderError::InvalidResponse(format!(
                "Codex App Server unavailable: {error}"
            )))
        })?;
    let client = handle.client;
    let mut notifications = client.subscribe();
    let mut server_requests = client.subscribe_requests();

    state
        .storage
        .update_turn_state(request.turn_id, TurnState::Processing)
        .await?;
    state
        .storage
        .update_state(request.thread_id, ThreadState::Processing)
        .await?;
    let _ = request.event_tx.send(AgentEvent::TurnStarted {
        turn_id: request.turn_id,
    });

    let tools = dynamic_tools(&request.dispatcher, &request.permissions);
    let tools_json = serde_json::to_string(&tools).unwrap_or_default();
    let developer_instructions = render_context(&request.assembled_context);
    let stored_thread_id = state.storage.get_codex_thread_id(request.thread_id).await?;
    let stored_tools_json = state
        .storage
        .get_codex_tools_json(request.thread_id)
        .await?;
    let codex_thread_id =
        match stored_thread_id.filter(|_| stored_tools_json.as_deref() == Some(&tools_json)) {
            Some(thread_id) => {
                let resume_request = json!({
                    "threadId": thread_id,
                    "developerInstructions": developer_instructions,
                });
                let _ = request.event_tx.send(AgentEvent::ProviderRequestPrepared {
                    turn_id: request.turn_id,
                    round: 0,
                    request: json!({"method":"thread/resume","params":resume_request}),
                });
                client
                    .request("thread/resume", resume_request)
                    .await
                    .map_err(provider_error)?;
                thread_id
            }
            None => {
                let start_request = json!({
                    "model": request.model,
                    "sandbox": sandbox_mode(&request.permissions),
                    "approvalPolicy": "on-request",
                    "cwd": request.agent_config.working_directory,
                    "developerInstructions": developer_instructions,
                    "ephemeral": false,
                    "dynamicTools": tools,
                });
                let _ = request.event_tx.send(AgentEvent::ProviderRequestPrepared {
                    turn_id: request.turn_id,
                    round: 0,
                    request: json!({"method":"thread/start","params":start_request}),
                });
                let response = client
                    .request("thread/start", start_request)
                    .await
                    .map_err(provider_error)?;
                let thread_id = response
                    .pointer("/thread/id")
                    .or_else(|| response.get("threadId"))
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        AgentError::ProviderFailed(swage_core::ProviderError::InvalidResponse(
                            "thread/start response omitted the thread id".to_string(),
                        ))
                    })?
                    .to_string();
                state
                    .storage
                    .set_codex_thread_id(request.thread_id, Some(&thread_id))
                    .await?;
                state
                    .storage
                    .set_codex_tools_json(request.thread_id, Some(&tools_json))
                    .await?;
                thread_id
            }
        };

    let sandbox_policy = sandbox_policy(&request.permissions, &request.agent_config.allowed_paths);
    let provider_request = json!({
        "threadId": codex_thread_id,
        "input": [{"type":"text", "text":request.user_message}],
        "sandboxPolicy": sandbox_policy,
        "approvalPolicy": "on-request",
        "cwd": request.agent_config.working_directory,
        "outputSchema": request.agent_config.output_schema,
    });
    let _ = request.event_tx.send(AgentEvent::ProviderRequestPrepared {
        turn_id: request.turn_id,
        round: 1,
        request: json!({"method":"turn/start","params":provider_request.clone()}),
    });
    let turn_response = client
        .request("turn/start", provider_request)
        .await
        .map_err(provider_error)?;
    let codex_turn_id = turn_response
        .pointer("/turn/id")
        .or_else(|| turn_response.get("turnId"))
        .and_then(Value::as_str)
        .map(ToString::to_string);

    let mut response_text = String::new();
    let mut input_tokens = 0u32;
    let mut output_tokens = 0u32;
    let mut tool_count = 0u32;
    let mut active_items = HashMap::<String, Value>::new();
    let mut tool_tasks = JoinSet::new();
    loop {
        tokio::select! {
            // Drain buffered requests and notifications before reporting
            // transport closure: the server may deliver a turn's terminal
            // notification and close in the same instant, and that outcome
            // must win over the close.
            biased;
            () = request.cancellation.cancelled() => {
                if let Some(turn_id) = codex_turn_id.as_deref() {
                    let _ = client.request("turn/interrupt", json!({"threadId":codex_thread_id,"turnId":turn_id})).await;
                }
                state.storage.update_turn_state(request.turn_id, TurnState::Interrupted).await?;
                state.storage.update_state(request.thread_id, ThreadState::Idle).await?;
                return Err(AgentError::Cancelled);
            }
            incoming = server_requests.recv() => {
                let incoming = incoming.map_err(|_| AgentError::ProviderFailed(swage_core::ProviderError::InvalidResponse(
                    "Codex App Server transport closed mid-turn".to_string(),
                )))?;
                if incoming.method == "item/tool/call" {
                    tool_count += 1;
                    let tool_state = state.clone();
                    let tool_client = client.clone();
                    let tool_request = request.clone();
                    let position = tool_count;
                    tool_tasks.spawn(async move {
                        handle_tool_call(&tool_state, &tool_client, &tool_request, incoming, position).await;
                    });
                } else {
                    handle_approval(&state, &client, &request, incoming, &active_items).await;
                }
            }
            incoming = notifications.recv() => {
                let incoming = incoming.map_err(|_| AgentError::ProviderFailed(swage_core::ProviderError::InvalidResponse(
                    "Codex App Server transport closed mid-turn".to_string(),
                )))?;
                match incoming.method.as_str() {
                    "item/started" | "item/updated" => {
                        let item = incoming.params.get("item").cloned().unwrap_or_else(|| incoming.params.clone());
                        if let Some(id) = item.get("id").and_then(Value::as_str) {
                            active_items.insert(id.to_string(), item);
                        }
                    }
                    "item/agentMessage/delta" => {
                        if let Some(delta) = incoming.params.get("delta").and_then(Value::as_str) {
                            response_text.push_str(delta);
                            let _ = request.event_tx.send(AgentEvent::Token { content: delta.to_string() });
                        }
                    }
                    "item/reasoning/summaryTextDelta" | "item/reasoning/textDelta" => {
                        if let Some(delta) = incoming.params.get("delta").and_then(Value::as_str) {
                            let _ = request.event_tx.send(AgentEvent::ProviderReasoningCaptured {
                                turn_id: request.turn_id,
                                round: 0,
                                content: delta.to_string(),
                                details: None,
                            });
                        }
                    }
                    "item/completed" => {
                        let item = incoming.params.get("item").cloned().unwrap_or_else(|| incoming.params.clone());
                        if item.get("type").and_then(Value::as_str) == Some("agentMessage")
                            && response_text.is_empty()
                            && let Some(text) = item.get("text").and_then(Value::as_str)
                        {
                            response_text = text.to_string();
                            let _ = request.event_tx.send(AgentEvent::Token { content: text.to_string() });
                        }
                        let _ = record_audit(&state.storage, request.session_id, request.turn_id, "codex_item", item.get("type").and_then(Value::as_str).unwrap_or("item"), json!({
                            "codex_thread_id":codex_thread_id,
                            "codex_turn_id":codex_turn_id,
                            "item":item,
                        })).await;
                    }
                    "thread/tokenUsage/updated" => {
                        let last = incoming.params.pointer("/tokenUsage/last").or_else(|| incoming.params.get("last"));
                        input_tokens = last.and_then(|v| v.get("inputTokens")).and_then(Value::as_u64).and_then(|v| u32::try_from(v).ok()).unwrap_or(input_tokens);
                        output_tokens = last.and_then(|v| v.get("outputTokens")).and_then(Value::as_u64).and_then(|v| u32::try_from(v).ok()).unwrap_or(output_tokens);
                    }
                    "turn/completed" => {
                        let status = incoming.params.pointer("/turn/status").and_then(Value::as_str).unwrap_or("failed");
                        if status != "completed" {
                            let message = incoming.params.pointer("/turn/error/message").and_then(Value::as_str).unwrap_or("Codex turn failed");
                            return Err(AgentError::ProviderFailed(swage_core::ProviderError::InvalidResponse(message.to_string())));
                        }
                        break;
                    }
                    _ => tracing::debug!(method = %incoming.method, "ignoring unsupported Codex notification"),
                }
            }
            () = client.closed() => {
                return Err(AgentError::ProviderFailed(swage_core::ProviderError::InvalidResponse(
                    "Codex App Server transport closed mid-turn".to_string(),
                )));
            }
        }
    }

    while let Some(result) = tool_tasks.join_next().await {
        if let Err(error) = result {
            tracing::warn!(%error, "Codex dynamic-tool dispatch task panicked");
        }
    }

    let structured_output = match request.agent_config.output_schema.as_ref() {
        Some(schema) => {
            let value: Value = serde_json::from_str(&response_text).map_err(|error| {
                AgentError::InvalidConfig(format!("Codex structured output was not JSON: {error}"))
            })?;
            let validator = jsonschema::validator_for(schema).map_err(|error| {
                AgentError::InvalidConfig(format!("invalid output schema: {error}"))
            })?;
            if validator.validate(&value).is_err() {
                return Err(AgentError::OutputSchemaFailed { attempts: 1 });
            }
            Some(value)
        }
        None => None,
    };
    state
        .storage
        .update_response(request.turn_id, &response_text)
        .await?;
    state
        .storage
        .update_tokens(
            request.turn_id,
            i64::from(input_tokens),
            i64::from(output_tokens),
        )
        .await?;
    state
        .storage
        .append_message(AppendMessage {
            thread_id: request.thread_id,
            turn_id: Some(request.turn_id),
            role: Role::Assistant,
            content: response_text.clone(),
            agent_id: request.agent_id.clone(),
        })
        .await?;
    state
        .storage
        .update_turn_state(request.turn_id, TurnState::Completed)
        .await?;
    state
        .storage
        .update_state(request.thread_id, ThreadState::Idle)
        .await?;
    let _ = request.event_tx.send(AgentEvent::TurnCompleted {
        turn_id: request.turn_id,
        input_tokens,
        output_tokens,
        tool_calls_count: tool_count,
        structured_output: structured_output.clone(),
    });
    Ok(OutboundMessage {
        response: response_text,
        turn_id: request.turn_id,
        input_tokens,
        output_tokens,
        tool_calls_count: tool_count,
        structured_output,
    })
}

fn dynamic_tools(
    dispatcher: &ToolDispatcher,
    permissions: &PermissionConfig,
) -> Vec<DynamicToolSpec> {
    dispatcher
        .tool_definitions()
        .into_iter()
        .filter(|definition| {
            definition
                .get("name")
                .and_then(Value::as_str)
                .is_some_and(|name| {
                    matches!(
                        check_permission(permissions, name),
                        Ok(PermissionCheckResult::Allowed)
                    )
                })
        })
        .filter_map(|definition| {
            Some(DynamicToolSpec::function(
                definition.get("name")?.as_str()?.to_string(),
                definition.get("description")?.as_str()?.to_string(),
                definition.get("parameters")?.clone(),
            ))
        })
        .collect()
}

fn render_context(messages: &[ChatMessage]) -> String {
    messages
        .iter()
        .map(|message| format!("{:?}: {}", message.role(), message.text_content()))
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn sandbox_mode(permissions: &PermissionConfig) -> &'static str {
    if permits(permissions, "shell")
        || permits(permissions, "file_write")
        || permits(permissions, "file_edit")
    {
        "workspace-write"
    } else {
        "read-only"
    }
}

fn sandbox_policy(permissions: &PermissionConfig, roots: &[std::path::PathBuf]) -> Value {
    if sandbox_mode(permissions) == "workspace-write" {
        json!({"type":"workspaceWrite","writableRoots":roots,"networkAccess":false,"excludeSlashTmp":true,"excludeTmpdirEnvVar":true})
    } else {
        json!({"type":"readOnly","networkAccess":false})
    }
}

fn permits(permissions: &PermissionConfig, tool: &str) -> bool {
    matches!(
        check_permission(permissions, tool),
        Ok(PermissionCheckResult::Allowed)
    )
}

async fn handle_tool_call(
    state: &AppState,
    client: &AppServerClient,
    request: &CodexTurnRequest,
    incoming: swage_codex::ServerRequest,
    position: u32,
) {
    let tool = incoming
        .params
        .get("tool")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let call_id = incoming
        .params
        .get("callId")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let arguments = incoming
        .params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let display_summary = request.dispatcher.display_summary(&tool, &arguments);
    let _ = request.event_tx.send(AgentEvent::ToolStart {
        call_id: call_id.clone(),
        tool_name: tool.clone(),
        arguments: arguments.clone(),
        display_summary: display_summary.clone(),
    });
    let started = Instant::now();
    let mut context = swage_agent::context::AgentToolContext::new(
        request.agent_id.clone(),
        request.session_id.to_string(),
        request.agent_config.working_directory.clone(),
    )
    .with_call_id(call_id.clone())
    .with_turn_id(request.turn_id)
    .with_thread_id(request.thread_id);
    context.allowed_paths = request.agent_config.allowed_paths.clone();
    let result = request
        .dispatcher
        .dispatch(&tool, arguments.clone(), &context, &request.agent_id)
        .await;
    let duration_ms = i64::try_from(started.elapsed().as_millis()).unwrap_or(i64::MAX);
    let (response, result_json, status) = match result {
        Ok(output) => {
            let text = serde_json::to_string(&output.result).unwrap_or_else(|_| "null".to_string());
            (
                ToolCallResponse {
                    success: true,
                    content_items: vec![ContentItem::text(text)],
                },
                output.result,
                "success",
            )
        }
        Err(error) => {
            let text = error.to_string();
            (
                ToolCallResponse {
                    success: false,
                    content_items: vec![ContentItem::text(text.clone())],
                },
                Value::String(text),
                "error",
            )
        }
    };
    let _ = state
        .storage
        .create_tool_call(NewToolCall {
            turn_id: request.turn_id,
            agent_id: request.agent_id.clone(),
            call_id: call_id.clone(),
            tool: tool.clone(),
            arguments: arguments.to_string(),
            model_arguments: arguments.to_string(),
            result: Some(result_json.to_string()),
            model_result: Some(result_json.to_string()),
            status: Some(status.to_string()),
            duration_ms: Some(duration_ms),
            truncated: false,
            total_bytes: None,
            provider_round: 0,
            position: i32::try_from(position.saturating_sub(1)).unwrap_or(i32::MAX),
            parent_call_id: None,
            display_summary,
        })
        .await;
    let _ = request.event_tx.send(AgentEvent::ToolEnd {
        call_id,
        tool_name: tool,
        result: result_json,
        status: status.to_string(),
        duration_ms: u64::try_from(duration_ms).ok(),
    });
    let _ = client
        .respond(
            incoming.id,
            serde_json::to_value(response).unwrap_or(Value::Null),
        )
        .await;
}

async fn handle_approval(
    state: &AppState,
    client: &AppServerClient,
    request: &CodexTurnRequest,
    incoming: swage_codex::ServerRequest,
    active_items: &HashMap<String, Value>,
) {
    let (decision, reason, response) = match incoming.method.as_str() {
        "item/commandExecution/requestApproval" => {
            let cwd_allowed = incoming
                .params
                .get("cwd")
                .and_then(Value::as_str)
                .is_none_or(|cwd| {
                    path_allowed(Path::new(cwd), &request.agent_config.allowed_paths)
                });
            let network = incoming
                .params
                .get("networkApprovalContext")
                .is_some_and(|value| !value.is_null());
            if permits(&request.permissions, "shell") && cwd_allowed && !network {
                (
                    "accept",
                    "shell allowed inside workspace",
                    json!({"decision":"accept"}),
                )
            } else {
                (
                    "decline",
                    "outside the shell, path, or network grant envelope",
                    json!({"decision":"decline"}),
                )
            }
        }
        "item/fileChange/requestApproval" => {
            let item = incoming
                .params
                .get("itemId")
                .and_then(Value::as_str)
                .and_then(|id| active_items.get(id));
            let paths = item
                .and_then(|item| item.get("changes"))
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|change| {
                    change
                        .get("path")
                        .or_else(|| change.get("filePath"))
                        .and_then(Value::as_str)
                })
                .collect::<Vec<_>>();
            let paths_allowed = !paths.is_empty()
                && paths
                    .iter()
                    .all(|path| path_allowed(Path::new(path), &request.agent_config.allowed_paths));
            if (permits(&request.permissions, "file_write")
                || permits(&request.permissions, "file_edit"))
                && paths_allowed
            {
                (
                    "accept",
                    "file mutation allowed inside writable roots",
                    json!({"decision":"accept"}),
                )
            } else {
                (
                    "decline",
                    "outside the file mutation grant envelope",
                    json!({"decision":"decline"}),
                )
            }
        }
        "item/permissions/requestApproval" => (
            "decline",
            "sandbox widening is never granted mid-turn",
            json!({"permissions":{"fileSystem":null,"network":null},"scope":"turn"}),
        ),
        _ => {
            let _ = record_audit(
                &state.storage,
                request.session_id,
                request.turn_id,
                "codex_approval",
                "unknown request kind denied",
                json!({
                    "request_kind":incoming.method,
                    "request":incoming.params,
                    "decision":"json_rpc_error",
                    "reason":"unknown request kind",
                }),
            )
            .await;
            let _ = client
                .respond_error(
                    incoming.id,
                    -32601,
                    "request kind is outside the granted envelope",
                )
                .await;
            return;
        }
    };
    let audit_result = record_audit(
        &state.storage,
        request.session_id,
        request.turn_id,
        "codex_approval",
        reason,
        json!({
            "request_kind":incoming.method,
            "request":incoming.params,
            "decision":decision,
            "reason":reason,
        }),
    )
    .await;
    // Approval without a durable audit record would make the harness path
    // less strict than native dispatch. Storage failure therefore narrows any
    // computed outcome to a denial; an already-denied request stays denied.
    let response = if audit_result.is_ok() {
        response
    } else {
        json!({"decision":"decline"})
    };
    let _ = client.respond(incoming.id, response).await;
}

fn path_allowed(path: &Path, roots: &[std::path::PathBuf]) -> bool {
    roots
        .iter()
        .any(|root| crate::tool::workspace::validate_path(root, &path.to_string_lossy()).is_ok())
}

async fn record_audit(
    storage: &Arc<dyn Storage>,
    session_id: Uuid,
    turn_id: Uuid,
    event_kind: &str,
    summary: &str,
    metadata: Value,
) -> Result<(), StorageError> {
    storage
        .create_audit_event(NewAuditEvent {
            id: Uuid::new_v4().to_string(),
            session_id: Some(session_id),
            entity_kind: "turn".to_string(),
            entity_id: turn_id.to_string(),
            event_kind: event_kind.to_string(),
            summary: summary.to_string(),
            metadata,
        })
        .await
        .map(|_| ())
}

fn provider_error(error: swage_codex::ClientError) -> AgentError {
    AgentError::ProviderFailed(swage_core::ProviderError::InvalidResponse(
        error.to_string(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use swage_core::ToolError;
    use swage_storage::NewTurn;
    use swage_tool::permission::RequireApprovalEntry;
    use swage_tool::{context::ToolContext, tool::Tool};

    struct NamedTool(&'static str);

    #[async_trait]
    impl Tool for NamedTool {
        fn name(&self) -> &str {
            self.0
        }
        fn description(&self) -> &str {
            "test tool"
        }
        fn parameters(&self) -> Value {
            json!({"type":"object"})
        }
        async fn execute(&self, params: Value, _ctx: &dyn ToolContext) -> Result<Value, ToolError> {
            Ok(params)
        }
    }

    fn permissions(allow: &[&str], approval: &[&str]) -> PermissionConfig {
        PermissionConfig {
            allow: allow.iter().map(|value| (*value).to_string()).collect(),
            deny: Vec::new(),
            bundles: Default::default(),
            require_approval: approval
                .iter()
                .map(|value| RequireApprovalEntry {
                    tool_name: (*value).to_string(),
                    timeout_secs: None,
                    timeout_policy: None,
                })
                .collect(),
        }
    }

    async fn run_scripted_turn(
        scenario: Value,
        dispatcher: ToolDispatcher,
        permissions: PermissionConfig,
    ) -> (
        Result<OutboundMessage, AgentError>,
        Vec<Value>,
        Arc<crate::turn::tests::MockStorage>,
    ) {
        run_scripted_turn_with_audit_failure(scenario, dispatcher, permissions, false).await
    }

    async fn run_scripted_turn_with_audit_failure(
        scenario: Value,
        dispatcher: ToolDispatcher,
        permissions: PermissionConfig,
        fail_audit_writes: bool,
    ) -> (
        Result<OutboundMessage, AgentError>,
        Vec<Value>,
        Arc<crate::turn::tests::MockStorage>,
    ) {
        let storage = Arc::new(crate::turn::tests::MockStorage::new());
        let (mut state, session_id) =
            crate::turn::tests::make_state_with_storage(Vec::new(), Arc::clone(&storage)).await;
        let thread = state
            .storage
            .list_by_session(session_id)
            .await
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
        let turn = state
            .storage
            .create_turn(NewTurn {
                thread_id: thread.id,
                agent_id: "gateway".to_string(),
                state: TurnState::Pending,
                user_input: Some("scripted turn".to_string()),
                parent_turn_id: None,
            })
            .await
            .unwrap();
        if fail_audit_writes {
            storage.fail_audit_writes();
        }
        let (client_side, server_side) = tokio::io::duplex(64 * 1024);
        let (client_read, client_write) = tokio::io::split(client_side);
        let (server_read, server_write) = tokio::io::split(server_side);
        let client =
            AppServerClient::new(client_write, client_read, std::time::Duration::from_secs(2));
        state.codex_supervisor = swage_codex::AppServerSupervisor::with_client(
            swage_codex::AppServerConfig::codex("/tmp/swage-codex-scenario".into()),
            client,
        );
        let mut server = tokio::spawn(async move {
            swage_codex::fake::run_scenario(
                &scenario,
                tokio::io::BufReader::new(server_read),
                server_write,
            )
            .await
        });
        let (event_tx, _events) = mpsc::unbounded_channel();
        let request = CodexTurnRequest {
            session_id,
            thread_id: thread.id,
            turn_id: turn.id,
            agent_id: "gateway".to_string(),
            model: "fake-codex".to_string(),
            user_message: "scripted turn".to_string(),
            assembled_context: Vec::new(),
            agent_config: AgentConfig::new("gateway").with_workspace("/work", vec!["/work".into()]),
            permissions,
            dispatcher: Arc::new(dispatcher),
            event_tx,
            cancellation: CancellationToken::new(),
        };
        let result =
            tokio::time::timeout(std::time::Duration::from_secs(3), run_turn(state, request))
                .await
                .expect("scripted turn timed out");
        let frames =
            match tokio::time::timeout(std::time::Duration::from_millis(100), &mut server).await {
                Ok(server) => server
                    .expect("scenario task panicked")
                    .expect("scenario mismatch"),
                Err(_elapsed) if result.is_ok() => {
                    server.abort();
                    Vec::new()
                }
                Err(elapsed) => {
                    panic!("scenario server did not finish ({elapsed}); turn result: {result:?}")
                }
            };
        (result, frames, storage)
    }

    fn successful_turn_notifications() -> Vec<Value> {
        vec![
            json!({"jsonrpc":"2.0","method":"item/agentMessage/delta","params":{"delta":"done"}}),
            json!({"jsonrpc":"2.0","method":"turn/completed","params":{"turn":{"id":"fake-turn","status":"completed","error":null}}}),
        ]
    }

    #[tokio::test]
    async fn scripted_malformed_frame_does_not_corrupt_the_turn_stream() {
        let notifications = successful_turn_notifications();
        let scenario = json!({"keepOpen":true,"steps":[
            {
                "receive":{"method":"thread/start"},
                "send":[{"jsonrpc":"2.0","id":"$request_id","result":{"thread":{"id":"fake-thread"}}}]
            },
            {
                "receive":{"method":"turn/start"},
                "send":[
                    {"jsonrpc":"2.0","id":"$request_id","result":{"turn":{"id":"fake-turn"}}},
                    {"raw":"malformed frame"},
                    notifications[0].clone(),
                    notifications[1].clone()
                ]
            }
        ]});
        let dispatcher = ToolDispatcher::allow_all(Vec::<Arc<dyn Tool>>::new());

        let (result, _, _) = run_scripted_turn(scenario, dispatcher, permissions(&[], &[])).await;
        assert_eq!(
            result
                .expect("valid frames after malformed input win")
                .response,
            "done"
        );
    }

    #[tokio::test]
    async fn scripted_mid_turn_server_death_fails_the_turn() {
        let scenario = json!({"steps":[
            {
                "receive":{"method":"thread/start"},
                "send":[{"jsonrpc":"2.0","id":"$request_id","result":{"thread":{"id":"fake-thread"}}}]
            },
            {
                "receive":{"method":"turn/start"},
                "send":[
                    {"jsonrpc":"2.0","id":"$request_id","result":{"turn":{"id":"fake-turn"}}},
                    {"jsonrpc":"2.0","method":"turn/started","params":{"turn":{"id":"fake-turn","status":"inProgress"}}}
                ],
                "close":true
            }
        ]});
        let dispatcher = ToolDispatcher::allow_all(Vec::<Arc<dyn Tool>>::new());

        let (result, _, _) = run_scripted_turn(scenario, dispatcher, permissions(&[], &[])).await;
        assert!(
            result
                .expect_err("child death must fail the turn")
                .to_string()
                .contains("transport closed")
        );
    }

    #[tokio::test]
    async fn failed_turn_completion_is_cleaned_up_once() {
        let scenario = json!({"steps":[
            {
                "receive":{"method":"thread/start"},
                "send":[{"jsonrpc":"2.0","id":"$request_id","result":{"thread":{"id":"fake-thread"}}}]
            },
            {
                "receive":{"method":"turn/start"},
                "send":[
                    {"jsonrpc":"2.0","id":"$request_id","result":{"turn":{"id":"fake-turn"}}},
                    {"jsonrpc":"2.0","method":"turn/completed","params":{"turn":{"id":"fake-turn","status":"failed","error":{"message":"scripted failure"}}}}
                ]
            }
        ]});
        let dispatcher = ToolDispatcher::allow_all(Vec::<Arc<dyn Tool>>::new());

        let (result, _, storage) =
            run_scripted_turn(scenario, dispatcher, permissions(&[], &[])).await;

        assert!(
            result
                .expect_err("failed completion must fail the turn")
                .to_string()
                .contains("scripted failure")
        );
        assert_eq!(
            storage.turn_state_write_calls(),
            2,
            "one processing write and one failed write must complete the lifecycle"
        );
    }

    #[tokio::test]
    async fn scripted_tool_call_offers_dispatcher_definitions_and_returns_result() {
        let dispatcher =
            ToolDispatcher::allow_all([Arc::new(NamedTool("file_read")) as Arc<dyn Tool>]);
        let expected_tools = json!([{
            "type":"function",
            "name":"file_read",
            "description":"test tool",
            "inputSchema":{"type":"object"}
        }]);
        let success_notifications = successful_turn_notifications();
        let scenario = json!({"keepOpen":true,"steps":[
            {
                "receive":{"method":"thread/start","params":{"dynamicTools":expected_tools}},
                "send":[{"jsonrpc":"2.0","id":"$request_id","result":{"thread":{"id":"fake-thread"}}}]
            },
            {
                "receive":{"method":"turn/start"},
                "send":[
                    {"jsonrpc":"2.0","id":"$request_id","result":{"turn":{"id":"fake-turn"}}},
                    {"jsonrpc":"2.0","id":"tool-request","method":"item/tool/call","params":{"tool":"file_read","callId":"call-1","arguments":{"path":"probe.txt"}}}
                ]
            },
            {
                "receiveExact":true,
                "receive":{"jsonrpc":"2.0","id":"tool-request","result":{"success":true,"contentItems":[{"type":"inputText","text":"{\"path\":\"probe.txt\"}"}]}},
                "send":success_notifications
            }
        ]});

        let (result, _, _) =
            run_scripted_turn(scenario, dispatcher, permissions(&["file_read"], &[])).await;
        let output = result.expect("scripted tool turn succeeds");
        assert_eq!(output.response, "done");
        assert_eq!(output.tool_calls_count, 1);
    }

    #[tokio::test]
    async fn scripted_approvals_enforce_grants_network_and_unknown_deny() {
        let final_notifications = successful_turn_notifications();
        let scenario = json!({"keepOpen":true,"steps":[
            {
                "receive":{"method":"thread/start"},
                "send":[{"jsonrpc":"2.0","id":"$request_id","result":{"thread":{"id":"fake-thread"}}}]
            },
            {
                "receive":{"method":"turn/start"},
                "send":[
                    {"jsonrpc":"2.0","id":"$request_id","result":{"turn":{"id":"fake-turn"}}},
                    {"jsonrpc":"2.0","id":"in-grant","method":"item/commandExecution/requestApproval","params":{"cwd":"/work"}}
                ]
            },
            {
                "receiveExact":true,
                "receive":{"jsonrpc":"2.0","id":"in-grant","result":{"decision":"accept"}},
                "send":[
                    {"jsonrpc":"2.0","method":"item/started","params":{"item":{"id":"outside-edit","type":"fileChange","changes":[{"path":"/outside/file"}]}}},
                    {"jsonrpc":"2.0","id":"out-of-grant","method":"item/fileChange/requestApproval","params":{"itemId":"outside-edit"}}
                ]
            },
            {
                "receiveExact":true,
                "receive":{"jsonrpc":"2.0","id":"out-of-grant","result":{"decision":"decline"}},
                "send":[{"jsonrpc":"2.0","id":"network","method":"item/commandExecution/requestApproval","params":{"cwd":"/work","networkApprovalContext":{"host":"example.test"}}}]
            },
            {
                "receiveExact":true,
                "receive":{"jsonrpc":"2.0","id":"network","result":{"decision":"decline"}},
                "send":[{"jsonrpc":"2.0","id":"unknown","method":"item/unknown/requestApproval","params":{}}]
            },
            {
                "receiveExact":true,
                "receive":{"jsonrpc":"2.0","id":"unknown","error":{"code":-32601,"message":"request kind is outside the granted envelope"}},
                "send":final_notifications
            }
        ]});
        let dispatcher = ToolDispatcher::allow_all(Vec::<Arc<dyn Tool>>::new());

        let (result, _, _) = run_scripted_turn(
            scenario,
            dispatcher,
            permissions(&["shell", "file_edit"], &[]),
        )
        .await;
        assert_eq!(result.expect("approval scenario succeeds").response, "done");
    }

    #[tokio::test]
    async fn scripted_approval_denies_when_the_audit_write_fails() {
        let final_notifications = successful_turn_notifications();
        let scenario = json!({"keepOpen":true,"steps":[
            {
                "receive":{"method":"thread/start"},
                "send":[{"jsonrpc":"2.0","id":"$request_id","result":{"thread":{"id":"fake-thread"}}}]
            },
            {
                "receive":{"method":"turn/start"},
                "send":[
                    {"jsonrpc":"2.0","id":"$request_id","result":{"turn":{"id":"fake-turn"}}},
                    {"jsonrpc":"2.0","id":"approval","method":"item/commandExecution/requestApproval","params":{"cwd":"/work"}}
                ]
            },
            {
                "receiveExact":true,
                "receive":{"jsonrpc":"2.0","id":"approval","result":{"decision":"decline"}},
                "send":final_notifications
            }
        ]});
        let dispatcher = ToolDispatcher::allow_all(Vec::<Arc<dyn Tool>>::new());

        let (result, _, _) = run_scripted_turn_with_audit_failure(
            scenario,
            dispatcher,
            permissions(&["shell"], &[]),
            true,
        )
        .await;
        assert_eq!(
            result
                .expect("audit failure must deny without breaking the turn")
                .response,
            "done"
        );
    }

    #[test]
    fn sandbox_defaults_to_read_only_without_mutation_grants() {
        let policy = sandbox_policy(&permissions(&["file_read"], &[]), &["/work".into()]);
        assert_eq!(policy["type"], "readOnly");
        assert_eq!(policy["networkAccess"], false);
    }

    #[test]
    fn workspace_write_never_opens_network_or_tmp() {
        let policy = sandbox_policy(&permissions(&["file_write"], &[]), &["/work".into()]);
        assert_eq!(policy["type"], "workspaceWrite");
        assert_eq!(policy["writableRoots"], json!(["/work"]));
        assert_eq!(policy["networkAccess"], false);
        assert_eq!(policy["excludeSlashTmp"], true);
        assert_eq!(policy["excludeTmpdirEnvVar"], true);
    }

    #[test]
    fn approval_gated_mutation_does_not_widen_sandbox() {
        let policy = sandbox_policy(
            &permissions(&["file_read"], &["file_write"]),
            &["/work".into()],
        );
        assert_eq!(policy["type"], "readOnly");
    }

    #[test]
    fn paths_outside_the_workspace_are_rejected() {
        assert!(path_allowed(Path::new("/work/file"), &["/work".into()]));
        assert!(!path_allowed(
            Path::new("/elsewhere/file"),
            &["/work".into()]
        ));
    }

    #[cfg(unix)]
    #[test]
    fn paths_beneath_a_symlinked_parent_outside_the_workspace_are_rejected() {
        use std::os::unix::fs::symlink;

        let workspace = tempfile::tempdir().expect("create workspace");
        let outside = tempfile::tempdir().expect("create outside directory");
        symlink(outside.path(), workspace.path().join("escape")).expect("create escape symlink");

        assert!(!path_allowed(
            &workspace.path().join("escape/new-file.txt"),
            &[workspace.path().to_path_buf()],
        ));
    }

    #[test]
    fn dynamic_tools_exclude_approval_gated_capabilities() {
        let dispatcher = ToolDispatcher::allow_all([
            Arc::new(NamedTool("file_read")) as Arc<dyn Tool>,
            Arc::new(NamedTool("authenticated_post")) as Arc<dyn Tool>,
        ]);
        let permissions = permissions(&["file_read"], &["authenticated_post"]);
        let names = dynamic_tools(&dispatcher, &permissions)
            .into_iter()
            .map(|tool| tool.name)
            .collect::<Vec<_>>();
        assert_eq!(names, ["file_read"]);
    }
}

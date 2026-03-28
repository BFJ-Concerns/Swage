//! HTTP client for the Swage gateway REST API.
//!
//! Wraps reqwest with typed methods covering the full gateway surface:
//! health checks, graceful shutdown, conversation CRUD, and project CRUD.
//! Constructed from a gateway address; also supports discovery from runtime
//! metadata so consumers don't need to know where the metadata lives.

use crate::error::ClientError;
use crate::runtime::RuntimeMetadata;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Project request / response types
// ---------------------------------------------------------------------------

/// Client-side view of a project returned by the gateway.
#[derive(Debug, Clone, Deserialize)]
pub struct ProjectResponse {
    pub id: Uuid,
    pub name: String,
    pub slug: String,
    pub workspace_path: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// Response from the list-projects endpoint.
#[derive(Debug, Clone, Deserialize)]
pub struct ProjectListResponse {
    pub projects: Vec<ProjectResponse>,
}

/// Request body for creating a new project.
#[derive(Debug, Serialize)]
pub struct CreateProjectRequest {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_path: Option<String>,
}

/// Request body for partially updating a project.
///
/// `None` fields are omitted from the JSON body — the gateway interprets an
/// absent field as "no change" rather than "clear this field".
#[derive(Debug, Serialize)]
pub struct UpdateProjectRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_path: Option<String>,
}

// ---------------------------------------------------------------------------
// GatewayClient
// ---------------------------------------------------------------------------

/// HTTP client for a running Swage gateway.
///
/// Constructed either directly with a known address or via `from_runtime()`,
/// which discovers the address from the runtime metadata file written on
/// gateway startup.
#[derive(Debug)]
pub struct GatewayClient {
    client: reqwest::Client,
    base_url: String,
}

impl GatewayClient {
    /// Constructs a client targeting the given gateway address.
    ///
    /// The `address` should include the scheme and host, e.g.
    /// `"http://127.0.0.1:3000"`. Trailing slashes are stripped so that
    /// path-joining in the individual methods is consistent.
    pub fn new(address: &str) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url: address.trim_end_matches('/').to_string(),
        }
    }

    /// Discovers the gateway address from runtime metadata and constructs a client.
    ///
    /// Returns `ClientError::GatewayNotRunning` if no metadata file exists or the
    /// recorded PID is no longer alive (crashed gateway with a stale metadata file).
    pub fn from_runtime() -> Result<Self, ClientError> {
        // Attempt to load the metadata file; a missing file means not running.
        let meta = RuntimeMetadata::load().map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                ClientError::GatewayNotRunning {
                    reason: "no metadata file found — gateway has not been started".to_string(),
                }
            } else {
                ClientError::MetadataIo { source: e }
            }
        })?;

        // A metadata file that references a dead PID means the gateway crashed.
        if meta.is_stale() {
            return Err(ClientError::GatewayNotRunning {
                reason: format!(
                    "metadata file exists but PID {} is no longer running (gateway crashed?)",
                    meta.pid
                ),
            });
        }

        Ok(Self::new(&meta.address))
    }

    /// Returns the base URL this client is configured against.
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Checks that the gateway is up and responding to requests.
    ///
    /// Calls `GET /api/health` and verifies the response contains
    /// `{"status": "ok"}`. Returns `Ok(())` on success.
    pub async fn health(&self) -> Result<(), ClientError> {
        let url = format!("{}/api/health", self.base_url);

        let response = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| self.map_reqwest_error(e, &url))?;

        // Validate HTTP status; get the response back for body parsing.
        let response = self.check_success(response, &url).await?;
        let status = response.status().as_u16();

        // Parse the response and verify the status field is "ok".
        let body: serde_json::Value = response.json().await.map_err(|e| {
            ClientError::RequestFailed {
                address: url.clone(),
                source: e,
            }
        })?;

        if body.get("status").and_then(|v| v.as_str()) == Some("ok") {
            Ok(())
        } else {
            Err(ClientError::UnexpectedResponse {
                address: url,
                status,
                body: body.to_string(),
            })
        }
    }

    /// Requests a graceful shutdown of the gateway.
    ///
    /// Calls `POST /api/shutdown`. The gateway drains in-flight requests
    /// before exiting; this method returns as soon as the request is accepted.
    pub async fn shutdown(&self) -> Result<(), ClientError> {
        let url = format!("{}/api/shutdown", self.base_url);

        let response = self
            .client
            .post(&url)
            .send()
            .await
            .map_err(|e| self.map_reqwest_error(e, &url))?;

        self.check_success(response, &url).await?;
        Ok(())
    }

    // --- Project CRUD ---

    /// Creates a new project.
    ///
    /// Calls `POST /api/projects` with a JSON body containing the project name
    /// and optional workspace path. Returns the created project on success.
    pub async fn create_project(
        &self,
        name: &str,
        workspace_path: Option<&str>,
    ) -> Result<ProjectResponse, ClientError> {
        let url = format!("{}/api/projects", self.base_url);
        let body = CreateProjectRequest {
            name: name.to_string(),
            workspace_path: workspace_path.map(str::to_string),
        };
        let response = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| self.map_reqwest_error(e, &url))?;
        self.parse_response(response, &url).await
    }

    /// Lists all projects.
    ///
    /// Calls `GET /api/projects`. Returns a list response wrapping all known
    /// projects.
    pub async fn list_projects(&self) -> Result<ProjectListResponse, ClientError> {
        let url = format!("{}/api/projects", self.base_url);
        let response = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| self.map_reqwest_error(e, &url))?;
        self.parse_response(response, &url).await
    }

    /// Fetches a single project by ID.
    ///
    /// Calls `GET /api/projects/{id}`. Returns `UnexpectedResponse` with HTTP 404
    /// if the project does not exist.
    pub async fn get_project(&self, id: Uuid) -> Result<ProjectResponse, ClientError> {
        let url = format!("{}/api/projects/{}", self.base_url, id);
        let response = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| self.map_reqwest_error(e, &url))?;
        self.parse_response(response, &url).await
    }

    /// Updates a project's name and/or workspace path.
    ///
    /// Calls `PUT /api/projects/{id}`. Fields set to `None` are omitted from the
    /// request body and left unchanged by the gateway.
    pub async fn update_project(
        &self,
        id: Uuid,
        name: Option<&str>,
        workspace_path: Option<&str>,
    ) -> Result<ProjectResponse, ClientError> {
        let url = format!("{}/api/projects/{}", self.base_url, id);
        let body = UpdateProjectRequest {
            name: name.map(str::to_string),
            workspace_path: workspace_path.map(str::to_string),
        };
        let response = self
            .client
            .put(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| self.map_reqwest_error(e, &url))?;
        self.parse_response(response, &url).await
    }

    /// Deletes a project by ID.
    ///
    /// Calls `DELETE /api/projects/{id}`. Returns `Ok(())` on success.
    pub async fn delete_project(&self, id: Uuid) -> Result<(), ClientError> {
        let url = format!("{}/api/projects/{}", self.base_url, id);

        let response = self
            .client
            .delete(&url)
            .send()
            .await
            .map_err(|e| self.map_reqwest_error(e, &url))?;

        self.check_success(response, &url).await?;
        Ok(())
    }

    /// Validates that a response has a 2xx status code.
    ///
    /// Returns the response unchanged on success so that callers that need the
    /// body can continue parsing. On non-2xx, reads the body as text and
    /// returns `ClientError::UnexpectedResponse`.
    async fn check_success(
        &self,
        response: reqwest::Response,
        url: &str,
    ) -> Result<reqwest::Response, ClientError> {
        if !response.status().is_success() {
            let status = response.status().as_u16();
            let body = response.text().await.unwrap_or_default();
            return Err(ClientError::UnexpectedResponse {
                address: url.to_string(),
                status,
                body,
            });
        }
        Ok(response)
    }

    /// Maps a `reqwest::Error` to the appropriate `ClientError` variant.
    ///
    /// Connection errors (refused, timeout before headers) become `ConnectionFailed`;
    /// all other transport errors become `RequestFailed`.
    fn map_reqwest_error(&self, err: reqwest::Error, url: &str) -> ClientError {
        if err.is_connect() || err.is_timeout() {
            ClientError::ConnectionFailed {
                address: url.to_string(),
                source: err,
            }
        } else {
            ClientError::RequestFailed {
                address: url.to_string(),
                source: err,
            }
        }
    }

    /// Parses a successful JSON response body into `T`, or converts HTTP/parse errors.
    async fn parse_response<T: for<'de> serde::Deserialize<'de>>(
        &self,
        response: reqwest::Response,
        url: &str,
    ) -> Result<T, ClientError> {
        if !response.status().is_success() {
            let status = response.status().as_u16();
            let body = response.text().await.unwrap_or_default();
            return Err(ClientError::UnexpectedResponse {
                address: url.to_string(),
                status,
                body,
            });
        }

        response.json::<T>().await.map_err(|e| {
            // reqwest surfaces JSON decode failures as a reqwest::Error, but the
            // semantic cause is a deserialisation failure — use the right variant.
            if e.is_decode() {
                ClientError::RequestFailed {
                    address: url.to_string(),
                    source: e,
                }
            } else {
                self.map_reqwest_error(e, url)
            }
        })
    }
}

// ---------------------------------------------------------------------------
// Conversation response types
// ---------------------------------------------------------------------------

/// Response returned when creating a conversation or querying its metadata.
#[derive(Debug, Clone, Deserialize)]
pub struct ConversationResponse {
    pub id: Uuid,
    pub project_id: Uuid,
    pub created_at: DateTime<Utc>,
    pub last_activity: DateTime<Utc>,
}

/// Compact summary of a conversation, used in list responses.
#[derive(Debug, Clone, Deserialize)]
pub struct ConversationSummary {
    pub id: Uuid,
    pub created_at: DateTime<Utc>,
    pub last_activity: DateTime<Utc>,
    /// First few words of the most recent message, if any.
    pub message_preview: Option<String>,
    /// Whether the most recent turn was interrupted before completing.
    pub has_interrupted_turn: bool,
}

/// Full conversation detail including message history.
#[derive(Debug, Clone, Deserialize)]
pub struct ConversationDetail {
    pub id: Uuid,
    pub project_id: Uuid,
    pub created_at: DateTime<Utc>,
    pub last_activity: DateTime<Utc>,
    pub messages: Vec<MessageView>,
}

/// A single message in a conversation, with its associated tool calls.
#[derive(Debug, Clone, Deserialize)]
pub struct MessageView {
    /// `"user"` or `"assistant"`.
    pub role: String,
    pub content: String,
    pub timestamp: DateTime<Utc>,
    /// Lifecycle state of the turn this message belongs to.
    pub turn_state: String,
    pub tool_calls: Vec<ToolCallView>,
}

/// A tool call made during a turn, with its result and execution metadata.
#[derive(Debug, Clone, Deserialize)]
pub struct ToolCallView {
    pub call_id: String,
    pub tool: String,
    pub arguments: serde_json::Value,
    pub result: Option<serde_json::Value>,
    pub status: Option<String>,
    pub duration_ms: Option<u64>,
    pub truncated: bool,
    pub total_bytes: Option<u64>,
    /// Set when this tool call was spawned by another tool call.
    pub parent_call_id: Option<String>,
}

/// Private wrapper for deserialising the `{"conversations": [...]}` list envelope.
#[derive(Deserialize)]
struct ConversationListEnvelope {
    conversations: Vec<ConversationSummary>,
}

// ---------------------------------------------------------------------------
// Conversation CRUD methods on GatewayClient
// ---------------------------------------------------------------------------

impl GatewayClient {
    /// Creates a new conversation inside a specific project, returning its metadata.
    ///
    /// Calls `POST /api/projects/{project_id}/sessions`. Returns 404 if the
    /// project does not exist.
    pub async fn create_project_conversation(
        &self,
        project_id: Uuid,
    ) -> Result<ConversationResponse, ClientError> {
        let url = format!("{}/api/projects/{project_id}/sessions", self.base_url);

        let response = self
            .client
            .post(&url)
            .send()
            .await
            .map_err(|e| self.map_reqwest_error(e, &url))?;

        self.parse_response(response, &url).await
    }

    /// Creates a new conversation, returning its metadata.
    ///
    /// Calls `POST /api/conversations`.
    pub async fn create_conversation(&self) -> Result<ConversationResponse, ClientError> {
        let url = format!("{}/api/conversations", self.base_url);

        let response = self
            .client
            .post(&url)
            .send()
            .await
            .map_err(|e| self.map_reqwest_error(e, &url))?;

        self.parse_response(response, &url).await
    }

    /// Returns summaries of all conversations, ordered by most recent activity.
    ///
    /// Calls `GET /api/conversations` and unwraps the `{"conversations": [...]}` envelope.
    pub async fn list_conversations(&self) -> Result<Vec<ConversationSummary>, ClientError> {
        let url = format!("{}/api/conversations", self.base_url);

        let response = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| self.map_reqwest_error(e, &url))?;

        let envelope: ConversationListEnvelope = self.parse_response(response, &url).await?;
        Ok(envelope.conversations)
    }

    /// Returns full details of a single conversation including its message history.
    ///
    /// Calls `GET /api/conversations/{id}`.
    pub async fn get_conversation(&self, id: Uuid) -> Result<ConversationDetail, ClientError> {
        let url = format!("{}/api/conversations/{id}", self.base_url);

        let response = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| self.map_reqwest_error(e, &url))?;

        self.parse_response(response, &url).await
    }

    /// Deletes a conversation and all associated messages.
    ///
    /// Calls `DELETE /api/conversations/{id}`.
    pub async fn delete_conversation(&self, id: Uuid) -> Result<(), ClientError> {
        let url = format!("{}/api/conversations/{id}", self.base_url);

        let response = self
            .client
            .delete(&url)
            .send()
            .await
            .map_err(|e| self.map_reqwest_error(e, &url))?;

        self.check_success(response, &url).await?;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Programmatic interface — async turns, messaging, approvals
    // -----------------------------------------------------------------------

    /// Submit a turn for async processing. Returns the turn_id.
    /// Calls `POST /api/turns`.
    pub async fn submit_turn(
        &self,
        conversation_id: Uuid,
        message: &str,
        output_schema: Option<serde_json::Value>,
    ) -> Result<Uuid, ClientError> {
        let url = format!("{}/api/turns", self.base_url);
        let body = serde_json::json!({
            "conversation_id": conversation_id,
            "message": message,
            "output_schema": output_schema,
        });

        let response = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| self.map_reqwest_error(e, &url))?;

        let response = self.check_success(response, &url).await?;
        let body: serde_json::Value = response.json().await.map_err(|e| {
            ClientError::RequestFailed {
                address: url.clone(),
                source: e,
            }
        })?;
        let turn_id = body["turn_id"]
            .as_str()
            .and_then(|s| Uuid::parse_str(s).ok())
            .ok_or_else(|| ClientError::UnexpectedResponse {
                address: url.clone(),
                status: 200,
                body: "missing or invalid turn_id in response".into(),
            })?;
        Ok(turn_id)
    }

    /// Poll a turn for status and result.
    /// Calls `GET /api/turns/{id}`.
    pub async fn poll_turn(&self, turn_id: Uuid) -> Result<serde_json::Value, ClientError> {
        let url = format!("{}/api/turns/{turn_id}", self.base_url);

        let response = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| self.map_reqwest_error(e, &url))?;

        let response = self.check_success(response, &url).await?;
        response.json().await.map_err(|e| {
            ClientError::RequestFailed {
                address: url.clone(),
                source: e,
            }
        })
    }

    /// Send a typed inter-agent message.
    /// Calls `POST /api/messages`.
    pub async fn send_agent_message(
        &self,
        sender_agent_id: &str,
        recipient_agent_id: &str,
        kind: &str,
        correlation_id: Option<Uuid>,
        payload: serde_json::Value,
    ) -> Result<serde_json::Value, ClientError> {
        let url = format!("{}/api/messages", self.base_url);
        let body = serde_json::json!({
            "sender_agent_id": sender_agent_id,
            "recipient_agent_id": recipient_agent_id,
            "kind": kind,
            "correlation_id": correlation_id,
            "payload": payload,
        });

        let response = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| self.map_reqwest_error(e, &url))?;

        let response = self.check_success(response, &url).await?;
        response.json().await.map_err(|e| {
            ClientError::RequestFailed {
                address: url.clone(),
                source: e,
            }
        })
    }

    /// Query message delivery status.
    /// Calls `GET /api/messages/{id}`.
    pub async fn get_message_status(
        &self,
        message_id: Uuid,
    ) -> Result<serde_json::Value, ClientError> {
        let url = format!("{}/api/messages/{message_id}", self.base_url);

        let response = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| self.map_reqwest_error(e, &url))?;

        let response = self.check_success(response, &url).await?;
        response.json().await.map_err(|e| {
            ClientError::RequestFailed {
                address: url.clone(),
                source: e,
            }
        })
    }

    /// List pending approval requests.
    /// Calls `GET /api/approvals`.
    pub async fn list_approvals(
        &self,
        agent_id: Option<&str>,
    ) -> Result<serde_json::Value, ClientError> {
        let mut url = format!("{}/api/approvals", self.base_url);
        if let Some(id) = agent_id {
            url.push_str(&format!("?agent_id={id}"));
        }

        let response = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| self.map_reqwest_error(e, &url))?;

        let response = self.check_success(response, &url).await?;
        response.json().await.map_err(|e| {
            ClientError::RequestFailed {
                address: url.clone(),
                source: e,
            }
        })
    }

    /// Decide on a pending approval (approve/reject).
    /// Calls `POST /api/approvals/{id}`.
    pub async fn decide_approval(
        &self,
        approval_id: Uuid,
        decision: &str,
        reason: Option<&str>,
    ) -> Result<serde_json::Value, ClientError> {
        let url = format!("{}/api/approvals/{approval_id}", self.base_url);
        let body = serde_json::json!({
            "decision": decision,
            "reason": reason,
        });

        let response = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| self.map_reqwest_error(e, &url))?;

        let response = self.check_success(response, &url).await?;
        response.json().await.map_err(|e| {
            ClientError::RequestFailed {
                address: url.clone(),
                source: e,
            }
        })
    }

    /// List turns in a conversation.
    /// Calls `GET /api/conversations/{id}/turns`.
    pub async fn list_turns(
        &self,
        conversation_id: Uuid,
        state: Option<&str>,
        agent_id: Option<&str>,
    ) -> Result<serde_json::Value, ClientError> {
        let mut url = format!("{}/api/conversations/{conversation_id}/turns", self.base_url);
        let mut params = vec![];
        if let Some(s) = state {
            params.push(format!("state={s}"));
        }
        if let Some(a) = agent_id {
            params.push(format!("agent_id={a}"));
        }
        if !params.is_empty() {
            url.push('?');
            url.push_str(&params.join("&"));
        }

        let response = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| self.map_reqwest_error(e, &url))?;

        let response = self.check_success(response, &url).await?;
        response.json().await.map_err(|e| {
            ClientError::RequestFailed {
                address: url.clone(),
                source: e,
            }
        })
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod project_type_tests {
    use super::*;

    const SAMPLE_PROJECT_JSON: &str = r#"{
        "id": "550e8400-e29b-41d4-a716-446655440000",
        "name": "My Project",
        "slug": "my-project",
        "workspace_path": "/home/user/projects",
        "created_at": "2024-01-01T00:00:00Z"
    }"#;

    #[test]
    fn project_response_deserialises_from_json() {
        let resp: ProjectResponse = serde_json::from_str(SAMPLE_PROJECT_JSON).unwrap();
        assert_eq!(resp.name, "My Project");
        assert_eq!(resp.slug, "my-project");
        assert_eq!(resp.workspace_path.as_deref(), Some("/home/user/projects"));
    }

    #[test]
    fn project_response_handles_null_workspace_path() {
        let json = r#"{
            "id": "550e8400-e29b-41d4-a716-446655440000",
            "name": "No Workspace",
            "slug": "no-workspace",
            "workspace_path": null,
            "created_at": "2024-01-01T00:00:00Z"
        }"#;
        let resp: ProjectResponse = serde_json::from_str(json).unwrap();
        assert!(resp.workspace_path.is_none());
    }

    #[test]
    fn project_list_response_deserialises_empty() {
        let json = r#"{"projects": []}"#;
        let resp: ProjectListResponse = serde_json::from_str(json).unwrap();
        assert!(resp.projects.is_empty());
    }

    #[test]
    fn project_list_response_deserialises_with_entries() {
        let json = format!(r#"{{"projects": [{SAMPLE_PROJECT_JSON}]}}"#);
        let resp: ProjectListResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(resp.projects.len(), 1);
        assert_eq!(resp.projects[0].slug, "my-project");
    }

    #[test]
    fn create_project_request_serialises_with_workspace() {
        let req = CreateProjectRequest {
            name: "My Project".to_string(),
            workspace_path: Some("/home/user/projects".to_string()),
        };
        let json: serde_json::Value = serde_json::to_value(&req).unwrap();
        assert_eq!(json["name"], "My Project");
        assert_eq!(json["workspace_path"], "/home/user/projects");
    }

    #[test]
    fn create_project_request_serialises_without_workspace() {
        let req = CreateProjectRequest {
            name: "Bare Project".to_string(),
            workspace_path: None,
        };
        let json: serde_json::Value = serde_json::to_value(&req).unwrap();
        assert_eq!(json["name"], "Bare Project");
        // None workspace should be absent from the serialised JSON, not null.
        assert!(json.get("workspace_path").is_none_or(|v| v.is_null()));
    }

    #[test]
    fn update_project_request_serialises_partial() {
        let req = UpdateProjectRequest {
            name: Some("New Name".to_string()),
            workspace_path: None,
        };
        let json: serde_json::Value = serde_json::to_value(&req).unwrap();
        assert_eq!(json["name"], "New Name");
        // workspace_path is None — skip_serializing_if = "Option::is_none" omits it.
        assert!(json.get("workspace_path").map_or(true, |v| v.is_null()));
    }

    #[test]
    fn update_project_request_serialises_both_fields() {
        let req = UpdateProjectRequest {
            name: Some("Renamed".to_string()),
            workspace_path: Some("/new/path".to_string()),
        };
        let json: serde_json::Value = serde_json::to_value(&req).unwrap();
        assert_eq!(json["name"], "Renamed");
        assert_eq!(json["workspace_path"], "/new/path");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // Shared crate-wide lock — prevents races with runtime.rs tests that also
    // manipulate XDG_RUNTIME_DIR during parallel test execution.
    use crate::test_helpers::ENV_LOCK;

    #[test]
    fn new_strips_trailing_slash() {
        let client = GatewayClient::new("http://127.0.0.1:3000/");
        assert_eq!(client.base_url(), "http://127.0.0.1:3000");
    }

    #[test]
    fn new_preserves_address_without_slash() {
        let client = GatewayClient::new("http://127.0.0.1:3000");
        assert_eq!(client.base_url(), "http://127.0.0.1:3000");
    }

    #[test]
    fn from_runtime_returns_gateway_not_running_when_no_metadata() {
        let _guard = ENV_LOCK.lock().unwrap();
        let tmpdir = tempfile::TempDir::new().unwrap();
        // SAFETY: single-threaded within the lock.
        unsafe { std::env::set_var("XDG_RUNTIME_DIR", tmpdir.path()) };
        let result = GatewayClient::from_runtime();
        unsafe { std::env::remove_var("XDG_RUNTIME_DIR") };

        match result {
            Err(ClientError::GatewayNotRunning { .. }) => {}
            other => panic!("expected GatewayNotRunning, got: {other:?}"),
        }
    }

    #[test]
    fn from_runtime_returns_gateway_not_running_for_stale_pid() {
        let _guard = ENV_LOCK.lock().unwrap();
        let tmpdir = tempfile::TempDir::new().unwrap();
        unsafe { std::env::set_var("XDG_RUNTIME_DIR", tmpdir.path()) };

        // 4_194_304 is Linux max_pid — never assigned to a real process.
        let meta = RuntimeMetadata {
            pid: 4_194_304,
            address: "http://127.0.0.1:3000".to_string(),
            started_at: chrono::Utc::now(),
        };
        meta.save().unwrap();
        let result = GatewayClient::from_runtime();
        unsafe { std::env::remove_var("XDG_RUNTIME_DIR") };

        #[cfg(target_os = "linux")]
        match result {
            Err(ClientError::GatewayNotRunning { .. }) => {}
            other => panic!("expected GatewayNotRunning for stale PID, got: {other:?}"),
        }
        // On non-Linux is_stale() always returns false, so the client succeeds.
        #[cfg(not(target_os = "linux"))]
        assert!(result.is_ok());
    }

    #[test]
    fn from_runtime_constructs_client_from_live_metadata() {
        let _guard = ENV_LOCK.lock().unwrap();
        let tmpdir = tempfile::TempDir::new().unwrap();
        unsafe { std::env::set_var("XDG_RUNTIME_DIR", tmpdir.path()) };

        // Use the current process PID — is_stale() will return false.
        let meta = RuntimeMetadata::new(
            std::process::id(),
            "http://127.0.0.1:3000".to_string(),
        );
        meta.save().unwrap();
        let result = GatewayClient::from_runtime();
        unsafe { std::env::remove_var("XDG_RUNTIME_DIR") };

        let client = result.expect("from_runtime should succeed with live PID metadata");
        assert_eq!(client.base_url(), "http://127.0.0.1:3000");
    }

    // health() and shutdown() require a live gateway — covered by integration tests.

    // -----------------------------------------------------------------------
    // Conversation DTO deserialization tests
    // -----------------------------------------------------------------------

    #[test]
    fn conversation_response_deserialises_from_gateway_json() {
        let id = Uuid::new_v4();
        let project_id = Uuid::new_v4();
        let json = serde_json::json!({
            "id": id,
            "project_id": project_id,
            "created_at": "2025-01-01T00:00:00Z",
            "last_activity": "2025-01-01T01:00:00Z",
        });

        let conv: ConversationResponse = serde_json::from_value(json).unwrap();
        assert_eq!(conv.id, id);
        assert_eq!(conv.project_id, project_id);
    }

    #[test]
    fn conversation_summary_deserialises_with_optional_preview() {
        let id = Uuid::new_v4();
        let json_with_preview = serde_json::json!({
            "id": id,
            "created_at": "2025-01-01T00:00:00Z",
            "last_activity": "2025-01-01T01:00:00Z",
            "message_preview": "Hello, world",
            "has_interrupted_turn": false,
        });
        let json_no_preview = serde_json::json!({
            "id": id,
            "created_at": "2025-01-01T00:00:00Z",
            "last_activity": "2025-01-01T01:00:00Z",
            "message_preview": null,
            "has_interrupted_turn": true,
        });

        let with_preview: ConversationSummary =
            serde_json::from_value(json_with_preview).unwrap();
        assert_eq!(with_preview.message_preview.as_deref(), Some("Hello, world"));
        assert!(!with_preview.has_interrupted_turn);

        let no_preview: ConversationSummary =
            serde_json::from_value(json_no_preview).unwrap();
        assert!(no_preview.message_preview.is_none());
        assert!(no_preview.has_interrupted_turn);
    }

    #[test]
    fn conversation_list_envelope_deserialises() {
        let json = serde_json::json!({
            "conversations": [
                {
                    "id": Uuid::new_v4(),
                    "created_at": "2025-01-01T00:00:00Z",
                    "last_activity": "2025-01-01T01:00:00Z",
                    "message_preview": null,
                    "has_interrupted_turn": false,
                }
            ]
        });

        let envelope: ConversationListEnvelope = serde_json::from_value(json).unwrap();
        assert_eq!(envelope.conversations.len(), 1);
    }

    #[test]
    fn conversation_detail_deserialises_with_messages() {
        let id = Uuid::new_v4();
        let json = serde_json::json!({
            "id": id,
            "project_id": Uuid::new_v4(),
            "created_at": "2025-01-01T00:00:00Z",
            "last_activity": "2025-01-01T01:00:00Z",
            "messages": [
                {
                    "role": "user",
                    "content": "Hello",
                    "timestamp": "2025-01-01T00:01:00Z",
                    "turn_state": "completed",
                    "tool_calls": [],
                }
            ],
        });

        let detail: ConversationDetail = serde_json::from_value(json).unwrap();
        assert_eq!(detail.id, id);
        assert_eq!(detail.messages.len(), 1);
        assert_eq!(detail.messages[0].role, "user");
    }

    #[test]
    fn tool_call_view_deserialises_with_optional_fields() {
        let json = serde_json::json!({
            "call_id": "tc-1",
            "tool": "read_file",
            "arguments": {"path": "/tmp/foo"},
            "result": null,
            "status": "completed",
            "duration_ms": 42,
            "truncated": false,
            "total_bytes": null,
            "parent_call_id": null,
        });

        let view: ToolCallView = serde_json::from_value(json).unwrap();
        assert_eq!(view.tool, "read_file");
        assert_eq!(view.duration_ms, Some(42));
        assert!(view.result.is_none());
    }
}

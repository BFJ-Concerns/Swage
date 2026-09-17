//! HTTP client for the Swage gateway REST API.
//!
//! Wraps reqwest with typed methods covering the full gateway surface:
//! health checks, graceful shutdown, conversation CRUD, and topic CRUD.
//! Constructed from a gateway address; also supports discovery from runtime
//! metadata so consumers don't need to know where the metadata lives.

use std::fmt::Write as _;

use crate::error::ClientError;
use crate::runtime::RuntimeMetadata;
use uuid::Uuid;

pub use swage_protocol::client::topic::{
    CreateTopicRequest, CreateWorkspaceAttachmentRequest, TopicListResponse, TopicResponse,
    UpdateTopicRequest, UpdateWorkspaceTrustRequest, WorkspaceAttachmentListResponse,
    WorkspaceAttachmentResponse,
};

pub use swage_protocol::client::approval::{ApprovalDecisionScope, ApprovalDecisionValue};
pub use swage_protocol::hosting::{
    ApprovalDecisionRequest, ClientPresenceResponse, DevicePresenceResponse,
    PresenceGatewayResponse, PresenceSnapshotResponse,
};

pub use swage_protocol::client::turn::{
    MessageResponse, SendMessageRequest, SubmitTurnRequest, SubmitTurnResponse, TurnPollResponse,
};
pub use swage_protocol::client::voice::{
    SynthesiseSpeechRequest, SynthesiseSpeechResponse, TranscribeAudioRequest,
    TranscribeAudioResponse, VoiceCapabilitiesResponse,
};

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
        let body: serde_json::Value =
            response
                .json()
                .await
                .map_err(|e| ClientError::RequestFailed {
                    address: url.clone(),
                    source: e,
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

    /// Fetches the runtime presence and liveness snapshot.
    pub async fn presence(&self) -> Result<PresenceSnapshotResponse, ClientError> {
        let url = format!("{}/api/presence", self.base_url);
        let response = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| self.map_reqwest_error(e, &url))?;
        self.parse_response(response, &url).await
    }

    /// Fetches voice support for the current runtime.
    pub async fn voice_capabilities(&self) -> Result<VoiceCapabilitiesResponse, ClientError> {
        let url = format!("{}/api/voice/capabilities", self.base_url);
        let response = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| self.map_reqwest_error(e, &url))?;
        self.parse_response(response, &url).await
    }

    /// Sends audio bytes for transcription.
    pub async fn transcribe_audio(
        &self,
        audio_bytes: Vec<u8>,
        mime_type: &str,
    ) -> Result<TranscribeAudioResponse, ClientError> {
        let url = format!("{}/api/voice/transcriptions", self.base_url);
        let response = self
            .client
            .post(&url)
            .json(&TranscribeAudioRequest {
                audio_bytes,
                mime_type: mime_type.to_string(),
            })
            .send()
            .await
            .map_err(|e| self.map_reqwest_error(e, &url))?;
        self.parse_response(response, &url).await
    }

    /// Synthesises speech audio from text.
    pub async fn synthesise_speech(
        &self,
        text: &str,
        voice: Option<&str>,
        format: Option<&str>,
    ) -> Result<SynthesiseSpeechResponse, ClientError> {
        let url = format!("{}/api/voice/speech", self.base_url);
        let response = self
            .client
            .post(&url)
            .json(&SynthesiseSpeechRequest {
                text: text.to_string(),
                voice: voice.map(str::to_string),
                format: format.map(str::to_string),
            })
            .send()
            .await
            .map_err(|e| self.map_reqwest_error(e, &url))?;
        self.parse_response(response, &url).await
    }

    // --- Topic CRUD ---

    /// Creates a new topic.
    ///
    /// Calls `POST /api/topics` with a JSON body containing the topic name.
    /// Returns the created topic on success.
    pub async fn create_topic(&self, name: &str) -> Result<TopicResponse, ClientError> {
        let url = format!("{}/api/topics", self.base_url);
        let body = CreateTopicRequest {
            name: name.to_string(),
            description: String::new(),
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

    /// Lists all topics.
    ///
    /// Calls `GET /api/topics`. Returns a list response wrapping all known
    /// topics.
    pub async fn list_topics(&self) -> Result<TopicListResponse, ClientError> {
        let url = format!("{}/api/topics", self.base_url);
        let response = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| self.map_reqwest_error(e, &url))?;
        self.parse_response(response, &url).await
    }

    /// Fetches a single topic by ID.
    ///
    /// Calls `GET /api/topics/{id}`. A missing topic answers in the wire
    /// error contract and surfaces as `ClientError::Gateway { code: NotFound, .. }`.
    pub async fn get_topic(&self, id: Uuid) -> Result<TopicResponse, ClientError> {
        let url = format!("{}/api/topics/{}", self.base_url, id);
        let response = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| self.map_reqwest_error(e, &url))?;
        self.parse_response(response, &url).await
    }

    /// Updates a topic's metadata.
    ///
    /// Calls `PUT /api/topics/{id}`. Fields set to `None` are omitted from the
    /// request body and left unchanged by the gateway.
    pub async fn update_topic(
        &self,
        id: Uuid,
        name: Option<&str>,
    ) -> Result<TopicResponse, ClientError> {
        let url = format!("{}/api/topics/{}", self.base_url, id);
        let body = UpdateTopicRequest {
            name: name.map(str::to_string),
            description: None,
            tags: None,
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

    pub async fn list_topic_workspaces(
        &self,
        topic_id: Uuid,
    ) -> Result<WorkspaceAttachmentListResponse, ClientError> {
        let url = format!("{}/api/topics/{}/workspaces", self.base_url, topic_id);
        let response = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| self.map_reqwest_error(e, &url))?;
        self.parse_response(response, &url).await
    }

    pub async fn attach_topic_workspace(
        &self,
        topic_id: Uuid,
        request: CreateWorkspaceAttachmentRequest,
    ) -> Result<WorkspaceAttachmentResponse, ClientError> {
        let url = format!("{}/api/topics/{}/workspaces", self.base_url, topic_id);
        let response = self
            .client
            .post(&url)
            .json(&request)
            .send()
            .await
            .map_err(|e| self.map_reqwest_error(e, &url))?;
        self.parse_response(response, &url).await
    }

    pub async fn update_topic_workspace_trust(
        &self,
        topic_id: Uuid,
        workspace_id: Uuid,
        is_trusted: bool,
    ) -> Result<WorkspaceAttachmentResponse, ClientError> {
        let url = format!(
            "{}/api/topics/{}/workspaces/{}/trust",
            self.base_url, topic_id, workspace_id
        );
        let response = self
            .client
            .put(&url)
            .json(&UpdateWorkspaceTrustRequest { is_trusted })
            .send()
            .await
            .map_err(|e| self.map_reqwest_error(e, &url))?;
        self.parse_response(response, &url).await
    }

    pub async fn detach_topic_workspace(
        &self,
        topic_id: Uuid,
        workspace_id: Uuid,
    ) -> Result<(), ClientError> {
        let url = format!(
            "{}/api/topics/{}/workspaces/{}",
            self.base_url, topic_id, workspace_id
        );
        let response = self
            .client
            .delete(&url)
            .send()
            .await
            .map_err(|e| self.map_reqwest_error(e, &url))?;
        self.check_success(response, &url).await?;
        Ok(())
    }

    /// Deletes a topic by ID.
    ///
    /// Calls `DELETE /api/topics/{id}`. Returns `Ok(())` on success.
    pub async fn delete_topic(&self, id: Uuid) -> Result<(), ClientError> {
        let url = format!("{}/api/topics/{}", self.base_url, id);

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
    /// body can continue parsing. On non-2xx, reads the body and converts it
    /// through [`refusal_error`].
    async fn check_success(
        &self,
        response: reqwest::Response,
        url: &str,
    ) -> Result<reqwest::Response, ClientError> {
        if !response.status().is_success() {
            return Err(refusal_error(response, url).await);
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
            return Err(refusal_error(response, url).await);
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

/// A non-2xx response as a `ClientError`: the wire error contract's body
/// becomes the typed [`ClientError::Gateway`]; anything else — a proxy's
/// HTML, an empty body — is kept raw as `UnexpectedResponse`.
async fn refusal_error(response: reqwest::Response, url: &str) -> ClientError {
    let status = response.status().as_u16();
    let body = response.text().await.unwrap_or_default();
    match serde_json::from_str::<swage_protocol::client::error::ApiErrorBody>(&body) {
        Ok(contract) => ClientError::Gateway {
            address: url.to_string(),
            code: contract.code,
            message: contract.message,
        },
        Err(_) => ClientError::UnexpectedResponse {
            address: url.to_string(),
            status,
            body,
        },
    }
}

use swage_protocol::client::conversation::ConversationListResponse as ConversationListEnvelope;
pub use swage_protocol::client::conversation::{
    ConversationDetail, ConversationResponse, ConversationSummary, CreateConversationRequest,
    MessageView, ToolCallView,
};

// ---------------------------------------------------------------------------
// Conversation CRUD methods on GatewayClient
// ---------------------------------------------------------------------------

impl GatewayClient {
    /// Creates a new conversation inside a specific topic, returning its metadata.
    ///
    /// Calls `POST /api/topics/{topic_id}/sessions`. Returns 404 if the
    /// topic does not exist.
    pub async fn create_topic_conversation(
        &self,
        topic_id: Uuid,
    ) -> Result<ConversationResponse, ClientError> {
        self.create_topic_conversation_with_model(topic_id, None)
            .await
    }

    pub async fn create_topic_conversation_with_model(
        &self,
        topic_id: Uuid,
        selected_model_id: Option<&str>,
    ) -> Result<ConversationResponse, ClientError> {
        self.create_topic_conversation_for_agent(topic_id, None, selected_model_id)
            .await
    }

    /// Creates a conversation in a topic, naming the agent definition the new
    /// session runs under. `None` selects the default agent; an unknown name
    /// is rejected by the gateway with a validation error.
    pub async fn create_topic_conversation_for_agent(
        &self,
        topic_id: Uuid,
        agent_id: Option<&str>,
        selected_model_id: Option<&str>,
    ) -> Result<ConversationResponse, ClientError> {
        let url = format!("{}/api/topics/{topic_id}/sessions", self.base_url);

        let mut request = self.client.post(&url);
        if agent_id.is_some() || selected_model_id.is_some() {
            request = request.json(&CreateConversationRequest {
                selected_model_id: selected_model_id.map(str::to_owned),
                agent_id: agent_id.map(str::to_owned),
                effort: None,
            });
        }
        let response = request
            .send()
            .await
            .map_err(|e| self.map_reqwest_error(e, &url))?;

        self.parse_response(response, &url).await
    }

    /// Creates a new conversation, returning its metadata.
    ///
    /// Calls `POST /api/conversations`.
    pub async fn create_conversation(&self) -> Result<ConversationResponse, ClientError> {
        self.create_conversation_with_model(None).await
    }

    pub async fn create_conversation_with_model(
        &self,
        selected_model_id: Option<&str>,
    ) -> Result<ConversationResponse, ClientError> {
        self.create_conversation_for_agent(None, selected_model_id)
            .await
    }

    /// Creates a conversation in the default topic, naming the agent
    /// definition the new session runs under. `None` selects the default
    /// agent; an unknown name is rejected by the gateway with a validation
    /// error.
    pub async fn create_conversation_for_agent(
        &self,
        agent_id: Option<&str>,
        selected_model_id: Option<&str>,
    ) -> Result<ConversationResponse, ClientError> {
        let url = format!("{}/api/conversations", self.base_url);

        let mut request = self.client.post(&url);
        if agent_id.is_some() || selected_model_id.is_some() {
            request = request.json(&CreateConversationRequest {
                selected_model_id: selected_model_id.map(str::to_owned),
                agent_id: agent_id.map(str::to_owned),
                effort: None,
            });
        }
        let response = request
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
        thread_id: Option<Uuid>,
        message: &str,
        output_schema: Option<serde_json::Value>,
    ) -> Result<Uuid, ClientError> {
        self.submit_turn_with_compaction_focus(
            conversation_id,
            thread_id,
            message,
            output_schema,
            None,
        )
        .await
    }

    /// Submit a turn with optional guidance for context summarisation.
    pub async fn submit_turn_with_compaction_focus(
        &self,
        conversation_id: Uuid,
        thread_id: Option<Uuid>,
        message: &str,
        output_schema: Option<serde_json::Value>,
        compaction_focus: Option<String>,
    ) -> Result<Uuid, ClientError> {
        let url = format!("{}/api/turns", self.base_url);
        let body = SubmitTurnRequest {
            conversation_id,
            thread_id,
            message: message.to_owned(),
            attachments: Vec::new(),
            output_schema,
            compaction_focus,
            model: None,
        };

        let response = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| self.map_reqwest_error(e, &url))?;

        let response = self.check_success(response, &url).await?;
        let body: SubmitTurnResponse =
            response
                .json()
                .await
                .map_err(|e| ClientError::RequestFailed {
                    address: url.clone(),
                    source: e,
                })?;
        Ok(body.turn_id)
    }

    /// Poll a turn for status and result.
    /// Calls `GET /api/turns/{id}`.
    pub async fn poll_turn(&self, turn_id: Uuid) -> Result<TurnPollResponse, ClientError> {
        let url = format!("{}/api/turns/{turn_id}", self.base_url);

        let response = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| self.map_reqwest_error(e, &url))?;

        let response = self.check_success(response, &url).await?;
        response
            .json()
            .await
            .map_err(|e| ClientError::RequestFailed {
                address: url.clone(),
                source: e,
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
    ) -> Result<MessageResponse, ClientError> {
        self.send_agent_message_with_topic(
            None,
            sender_agent_id,
            recipient_agent_id,
            kind,
            correlation_id,
            payload,
        )
        .await
    }

    /// Send a typed inter-agent message scoped to a topic.
    /// Calls `POST /api/messages`.
    pub async fn send_agent_message_in_topic(
        &self,
        topic_id: Uuid,
        sender_agent_id: &str,
        recipient_agent_id: &str,
        kind: &str,
        correlation_id: Option<Uuid>,
        payload: serde_json::Value,
    ) -> Result<MessageResponse, ClientError> {
        self.send_agent_message_with_topic(
            Some(topic_id),
            sender_agent_id,
            recipient_agent_id,
            kind,
            correlation_id,
            payload,
        )
        .await
    }

    async fn send_agent_message_with_topic(
        &self,
        topic_id: Option<Uuid>,
        sender_agent_id: &str,
        recipient_agent_id: &str,
        kind: &str,
        correlation_id: Option<Uuid>,
        payload: serde_json::Value,
    ) -> Result<MessageResponse, ClientError> {
        let url = format!("{}/api/messages", self.base_url);
        let body = SendMessageRequest {
            sender_agent_id: sender_agent_id.to_owned(),
            recipient_agent_id: recipient_agent_id.to_owned(),
            kind: kind.to_owned(),
            correlation_id,
            topic_id,
            sender_turn_id: None,
            payload,
        };

        let response = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| self.map_reqwest_error(e, &url))?;

        let response = self.check_success(response, &url).await?;
        response
            .json()
            .await
            .map_err(|e| ClientError::RequestFailed {
                address: url.clone(),
                source: e,
            })
    }

    /// Query message delivery status.
    /// Calls `GET /api/messages/{id}`.
    pub async fn get_message_status(
        &self,
        message_id: Uuid,
    ) -> Result<MessageResponse, ClientError> {
        let url = format!("{}/api/messages/{message_id}", self.base_url);

        let response = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| self.map_reqwest_error(e, &url))?;

        let response = self.check_success(response, &url).await?;
        response
            .json()
            .await
            .map_err(|e| ClientError::RequestFailed {
                address: url.clone(),
                source: e,
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
            let _ = write!(url, "?agent_id={id}");
        }

        let response = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| self.map_reqwest_error(e, &url))?;

        let response = self.check_success(response, &url).await?;
        response
            .json()
            .await
            .map_err(|e| ClientError::RequestFailed {
                address: url.clone(),
                source: e,
            })
    }

    /// Decide on a pending approval (approve/reject).
    /// Calls `POST /api/approvals/{id}`.
    pub async fn decide_approval(
        &self,
        approval_id: Uuid,
        decision: ApprovalDecisionValue,
        reason: Option<&str>,
    ) -> Result<serde_json::Value, ClientError> {
        self.decide_approval_with_scope(approval_id, decision, ApprovalDecisionScope::Once, reason)
            .await
    }

    /// Decide on a pending approval with an explicit scope.
    /// Calls `POST /api/approvals/{id}`.
    pub async fn decide_approval_with_scope(
        &self,
        approval_id: Uuid,
        decision: ApprovalDecisionValue,
        scope: ApprovalDecisionScope,
        reason: Option<&str>,
    ) -> Result<serde_json::Value, ClientError> {
        let url = format!("{}/api/approvals/{approval_id}", self.base_url);
        let body = ApprovalDecisionRequest {
            decision,
            scope,
            reason: reason.map(str::to_owned),
        };

        let response = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| self.map_reqwest_error(e, &url))?;

        let response = self.check_success(response, &url).await?;
        response
            .json()
            .await
            .map_err(|e| ClientError::RequestFailed {
                address: url.clone(),
                source: e,
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
        let mut url = format!(
            "{}/api/conversations/{conversation_id}/turns",
            self.base_url
        );
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
        response
            .json()
            .await
            .map_err(|e| ClientError::RequestFailed {
                address: url.clone(),
                source: e,
            })
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod topic_type_tests {
    use super::*;

    const SAMPLE_TOPIC_JSON: &str = r#"{
        "id": "550e8400-e29b-41d4-a716-446655440000",
        "name": "My Topic",
        "description": "A workspace",
        "slug": "my-topic",
        "created_at": "2024-01-01T00:00:00Z"
    }"#;

    #[test]
    fn topic_response_deserialises_from_json() {
        let resp: TopicResponse = serde_json::from_str(SAMPLE_TOPIC_JSON).unwrap();
        assert_eq!(resp.name, "My Topic");
        assert_eq!(resp.slug, "my-topic");
    }

    #[test]
    fn topic_list_response_deserialises_empty() {
        let json = r#"{"topics": []}"#;
        let resp: TopicListResponse = serde_json::from_str(json).unwrap();
        assert!(resp.topics.is_empty());
    }

    #[test]
    fn topic_list_response_deserialises_with_entries() {
        let json = format!(r#"{{"topics": [{SAMPLE_TOPIC_JSON}]}}"#);
        let resp: TopicListResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(resp.topics.len(), 1);
        assert_eq!(resp.topics[0].slug, "my-topic");
    }

    #[test]
    fn create_topic_request_serialises_name_only() {
        let req = CreateTopicRequest {
            name: "My Topic".to_string(),
            description: String::new(),
        };
        let json: serde_json::Value = serde_json::to_value(&req).unwrap();
        assert_eq!(json["name"], "My Topic");
    }

    #[test]
    fn update_topic_request_serialises_partial() {
        let req = UpdateTopicRequest {
            name: Some("New Name".to_string()),
            description: None,
            tags: None,
        };
        let json: serde_json::Value = serde_json::to_value(&req).unwrap();
        assert_eq!(json["name"], "New Name");
    }

    #[test]
    fn workspace_attachment_request_serialises_scope() {
        let req = UpdateTopicRequest {
            name: Some("Renamed".to_string()),
            description: None,
            tags: None,
        };
        let json: serde_json::Value = serde_json::to_value(&req).unwrap();
        assert_eq!(json["name"], "Renamed");

        let workspace = CreateWorkspaceAttachmentRequest {
            path: "/new/path".to_string(),
            label: Some("Notes".to_string()),
            set_as_default: false,
            active_session_id: None,
            permissions: vec!["read".to_string()],
            is_trusted: false,
        };
        let json: serde_json::Value = serde_json::to_value(&workspace).unwrap();
        assert_eq!(json["path"], "/new/path");
        assert_eq!(json["label"], "Notes");
        assert_eq!(json["permissions"][0], "read");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorCode;
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
        let meta = RuntimeMetadata::new(std::process::id(), "http://127.0.0.1:3000".to_string());
        meta.save().unwrap();
        let result = GatewayClient::from_runtime();
        unsafe { std::env::remove_var("XDG_RUNTIME_DIR") };

        let client = result.expect("from_runtime should succeed with live PID metadata");
        assert_eq!(client.base_url(), "http://127.0.0.1:3000");
    }

    // health() and shutdown() require a live gateway — covered by integration tests.

    /// Serve one canned HTTP response on a loopback listener and return the
    /// base URL. The listener answers exactly one request, then closes.
    async fn one_shot_server(status_line: &'static str, body: &'static str) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind loopback listener");
        let address = listener.local_addr().expect("listener address");
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept one request");
            let mut request = [0u8; 4096];
            let _ = stream.read(&mut request).await;
            let response = format!(
                "HTTP/1.1 {status_line}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            stream
                .write_all(response.as_bytes())
                .await
                .expect("write response");
        });
        format!("http://{address}")
    }

    #[tokio::test]
    async fn a_contract_refusal_becomes_a_typed_gateway_error() {
        let base = one_shot_server(
            "409 Conflict",
            r#"{"code":"turn_in_progress","message":"turn already in progress for session 1"}"#,
        )
        .await;
        let error = GatewayClient::new(&base)
            .list_topics()
            .await
            .expect_err("a refused request fails");
        match error {
            ClientError::Gateway {
                address,
                code,
                message,
            } => {
                assert_eq!(address, format!("{base}/api/topics"));
                assert_eq!(code, ErrorCode::TurnInProgress);
                assert_eq!(message, "turn already in progress for session 1");
            }
            other => panic!("expected a typed gateway refusal, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_refusal_outside_the_contract_keeps_its_raw_body() {
        let base = one_shot_server("502 Bad Gateway", "<html>upstream down</html>").await;
        let error = GatewayClient::new(&base)
            .list_topics()
            .await
            .expect_err("a refused request fails");
        match error {
            ClientError::UnexpectedResponse { status, body, .. } => {
                assert_eq!(status, 502);
                assert_eq!(body, "<html>upstream down</html>");
            }
            other => panic!("expected the raw body to be kept, got: {other:?}"),
        }
    }

    // -----------------------------------------------------------------------
    // Conversation DTO deserialization tests
    // -----------------------------------------------------------------------

    #[test]
    fn conversation_response_deserialises_from_gateway_json() {
        let id = Uuid::new_v4();
        let topic_id = Uuid::new_v4();
        let json = serde_json::json!({
            "id": id,
            "topic_id": topic_id,
            "agent_id": "gateway",
            "created_at": "2025-01-01T00:00:00Z",
            "last_activity": "2025-01-01T01:00:00Z",
        });

        let conv: ConversationResponse = serde_json::from_value(json).unwrap();
        assert_eq!(conv.id, id);
        assert_eq!(conv.topic_id, Some(topic_id));
    }

    #[test]
    fn conversation_summary_deserialises_with_optional_preview() {
        let id = Uuid::new_v4();
        let json_with_preview = serde_json::json!({
            "id": id,
            "agent_id": "gateway",
            "created_at": "2025-01-01T00:00:00Z",
            "last_activity": "2025-01-01T01:00:00Z",
            "message_preview": "Hello, world",
            "has_interrupted_turn": false,
        });
        let json_no_preview = serde_json::json!({
            "id": id,
            "agent_id": "gateway",
            "created_at": "2025-01-01T00:00:00Z",
            "last_activity": "2025-01-01T01:00:00Z",
            "message_preview": null,
            "has_interrupted_turn": true,
        });

        let with_preview: ConversationSummary = serde_json::from_value(json_with_preview).unwrap();
        assert_eq!(
            with_preview.message_preview.as_deref(),
            Some("Hello, world")
        );
        assert!(!with_preview.has_interrupted_turn);

        let no_preview: ConversationSummary = serde_json::from_value(json_no_preview).unwrap();
        assert!(no_preview.message_preview.is_none());
        assert!(no_preview.has_interrupted_turn);
    }

    #[test]
    fn conversation_list_envelope_deserialises() {
        let json = serde_json::json!({
            "conversations": [
                {
                    "id": Uuid::new_v4(),
                    "agent_id": "gateway",
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
            "topic_id": Uuid::new_v4(),
            "agent_id": "gateway",
            "created_at": "2025-01-01T00:00:00Z",
            "last_activity": "2025-01-01T01:00:00Z",
            "messages": [
                {
                    "thread_id": Uuid::new_v4(),
                    "turn_id": Uuid::new_v4(),
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

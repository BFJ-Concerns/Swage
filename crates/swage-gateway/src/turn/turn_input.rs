//! What a caller submits for a turn, and how its attachments are admitted,
//! stored, and shown to the model.
//!
//! Placed here because it has one responsibility a reader can hold without
//! the coordinator: the submitted input's contract — the text and the
//! attachments beside it — admitted against the effective model's provider
//! before anything is persisted, recorded verbatim in the wire
//! representation, and rendered as the model-facing content parts. Every
//! entry point admits through `resolve_turn_model_and_attachments`, so the
//! admission decision cannot drift from execution.

use swage_storage::MessageOrigin;

use super::{
    AppState, ContentPart, GatewayError, MessageAttachment, Uuid, apply_agent_definition,
    apply_dispatch_bounds, apply_dispatch_task_bounds, apply_task_definition,
    apply_workflow_call_tools, dispatch_bound_agent_ids, dispatch_bound_task_definition_ids,
    effective_session_config, ensure_selected_model_available, load_session_config,
    resolve_model_catalogue_entry, resolve_requested_turn_model,
};

/// What a caller submits for one turn: the message text, the content parts
/// sent beside it, and who composed it. Every turn entry point takes this; a
/// `String` converts as a person's own message, so a text-only caller passes
/// its text unchanged, and a platform path names its origin with
/// [`TurnInput::with_origin`] so the recorded row says a person did not type it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnInput {
    /// Correlates this send with its client's optimistic row.
    pub client_id: Option<Uuid>,
    pub text: String,
    pub attachments: Vec<MessageAttachment>,
    pub origin: MessageOrigin,
}

impl Default for TurnInput {
    fn default() -> Self {
        Self::text("")
    }
}

impl TurnInput {
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            client_id: None,
            text: text.into(),
            attachments: Vec::new(),
            origin: MessageOrigin::User,
        }
    }

    pub fn with_attachments(text: impl Into<String>, attachments: Vec<MessageAttachment>) -> Self {
        Self {
            client_id: None,
            text: text.into(),
            attachments,
            origin: MessageOrigin::User,
        }
    }

    pub fn with_client_id(mut self, client_id: Option<Uuid>) -> Self {
        self.client_id = client_id;
        self
    }

    /// Name the platform path that composed this input — a schedule's brief,
    /// a workflow's prompt — so the stored message and every open view carry
    /// the fact. A person's own message keeps the default.
    pub fn with_origin(mut self, origin: MessageOrigin) -> Self {
        self.origin = origin;
        self
    }
}

impl From<String> for TurnInput {
    fn from(text: String) -> Self {
        Self::text(text)
    }
}

impl From<&str> for TurnInput {
    fn from(text: &str) -> Self {
        Self::text(text)
    }
}

/// The content parts a stored message carries, parsed against the wire
/// contract. Storage keeps attachments as opaque JSON; a value outside the
/// contract is a corrupt record and is refused loudly here, never dropped.
pub(crate) fn message_attachments(
    stored: &[serde_json::Value],
) -> Result<Vec<MessageAttachment>, GatewayError> {
    stored
        .iter()
        .map(|value| {
            serde_json::from_value(value.clone()).map_err(|error| {
                GatewayError::Storage(swage_core::StorageError::Corruption {
                    message: format!("message attachment outside the wire contract: {error}"),
                })
            })
        })
        .collect()
}

/// The attachment values as storage records them: the wire representation
/// verbatim, so the stored row and the client contract never disagree.
pub(crate) fn attachment_records(attachments: &[MessageAttachment]) -> Vec<serde_json::Value> {
    attachments
        .iter()
        .map(|attachment| serde_json::to_value(attachment).expect("attachments serialise"))
        .collect()
}

/// The model-facing content parts for a submitted message: the text first,
/// then one part per attachment in submission order.
pub(crate) fn content_parts(text: String, attachments: &[MessageAttachment]) -> Vec<ContentPart> {
    let mut parts = Vec::with_capacity(1 + attachments.len());
    parts.push(ContentPart::Text { text });
    parts.extend(attachments.iter().map(|attachment| match attachment {
        MessageAttachment::ImageUrl { url } => ContentPart::ImageUrl { url: url.clone() },
    }));
    parts
}

/// Why an attachment was refused, with the scheme when the operator's rule refused it.
#[derive(Debug)]
pub(crate) struct AttachmentRefusal {
    reason: String,
    refused_scheme: Option<String>,
}

impl From<AttachmentRefusal> for GatewayError {
    fn from(refusal: AttachmentRefusal) -> Self {
        Self::ValidationError(refusal.reason)
    }
}

/// Admit each image reference against the operator's policy before checking
/// whether the selected provider can consume it. Nothing is persisted yet.
pub(crate) fn check_attachment_support(
    capabilities: &crate::capabilities::WorkflowCapabilities,
    policy: &swage_config::AttachmentsConfig,
    attachments: &[MessageAttachment],
) -> Result<(), AttachmentRefusal> {
    for attachment in attachments {
        match attachment {
            MessageAttachment::ImageUrl { url } => {
                let scheme = url.split_once(':').map(|(scheme, _)| scheme);
                if !scheme.is_some_and(|scheme| {
                    policy
                        .accepted_schemes
                        .iter()
                        .any(|accepted| accepted.eq_ignore_ascii_case(scheme))
                }) {
                    let scheme = scheme.unwrap_or("<missing>").to_owned();
                    return Err(AttachmentRefusal {
                        reason: format!(
                            "image reference scheme `{scheme}` is not accepted; accepted schemes: {}",
                            policy.accepted_schemes.join(", ")
                        ),
                        refused_scheme: Some(scheme),
                    });
                }
                let reason = if !capabilities.vision() {
                    Some("the selected provider does not accept image attachments")
                } else if !capabilities.accepts_image_reference(url) {
                    Some(
                        "the selected provider cannot resolve this image reference: attach the image inline as a base64 data URL",
                    )
                } else {
                    None
                };
                if let Some(reason) = reason {
                    return Err(AttachmentRefusal {
                        reason: reason.into(),
                        refused_scheme: None,
                    });
                }
            }
        }
    }
    Ok(())
}

/// Resolve the conversation's configuration for admission and execution.
/// Both callers apply the file hierarchy, agent definition and dispatch bounds
/// before deciding whether the selected provider can consume the input.
pub(crate) async fn resolve_turn_admission_config(
    state: &AppState,
    scope: &swage_core::ScopeLocation,
    session: &swage_storage::Session,
) -> Result<(swage_config::ResolvedConfig, bool), GatewayError> {
    let topic_id = session.topic_id;
    let session_id = session.id;
    // Load topic-specific config. Missing topic config falls back to the
    // startup config, but present invalid config fails the turn before any
    // user message is written.
    let config = load_session_config(state, scope, &session.agent_id)?;

    // A durable agent definition, when one exists for this agent, gives the
    // session its character on top of the resolved file config: instructions
    // append, the model preference fills an unselected default, and the
    // permission overlay narrows grants. Resolution failure falls open to
    // the file config — a broken definition must not brick the session.
    let config = match crate::service::resolve_effective_agent_definition(
        state.storage.as_ref(),
        &session.agent_id,
        topic_id,
    )
    .await
    {
        Ok(Some(definition)) => apply_agent_definition(config, &definition.definition),
        Ok(None) => config,
        Err(error) => {
            tracing::warn!(
                agent_id = %session.agent_id,
                %error,
                "failed to resolve the agent definition; using file config alone"
            );
            config
        }
    };

    // A worker session carries the full lineage of dispatching contexts in
    // session metadata: the worker's grants are the intersection of its own
    // and every inherited bound, re-resolved on every turn so nested dispatch
    // cannot expand authority and suspension cannot freeze it.
    // Unlike the definition overlay above, resolution failure here fails the
    // turn: falling open would run the worker wider than its dispatcher.
    let bound_agent_ids = dispatch_bound_agent_ids(session.metadata.as_ref())?;
    let config = apply_dispatch_bounds(
        state,
        scope,
        topic_id,
        config,
        &bound_agent_ids,
        &format!("worker session {session_id}"),
    )
    .await?;
    let bound_task_definition_ids = dispatch_bound_task_definition_ids(session.metadata.as_ref())?;
    let config = apply_dispatch_task_bounds(
        state,
        config,
        &bound_task_definition_ids,
        &format!("worker session {session_id}"),
    )
    .await?;
    let config = apply_workflow_call_tools(config, session.metadata.as_ref())?;

    // Task names ride child-session metadata, while the definition is
    // resolved afresh for every execution. Missing or unreadable tasks fail
    // closed: silently running a different contract would be false success.
    let task_definition_id = session
        .metadata
        .as_ref()
        .and_then(|m| m.get("task_definition"))
        .and_then(serde_json::Value::as_str);
    let (config, task_defines_output_retries) = match task_definition_id {
        Some(task_id) => {
            let definition =
                crate::service::resolve_task_definition(state.storage.as_ref(), task_id).await?;
            let defines_output_retries = definition.output_max_retries.is_some();
            (
                apply_task_definition(config, &definition)?,
                defines_output_retries,
            )
        }
        None => (config, false),
    };

    Ok((config, task_defines_output_retries))
}

/// Resolve the request's effective model and admit its attachments against
/// that model's provider protocol. Every entry point uses this seam before it
/// persists a turn, so the admission decision cannot drift from execution.
pub(crate) async fn resolve_turn_model_and_attachments(
    state: &AppState,
    config: swage_config::ResolvedConfig,
    requested_model: Option<&str>,
    session_selected_model: Option<&str>,
    session_id: Uuid,
    attachments: &[MessageAttachment],
) -> Result<(swage_config::ResolvedConfig, &'static str, Option<String>), GatewayError> {
    let selected_model_id = requested_model.or(session_selected_model);
    if let Some(requested_model) = requested_model {
        resolve_requested_turn_model(&config, requested_model)?;
    }
    ensure_selected_model_available(state, &config, selected_model_id, session_id)?;
    let effective_model_source = match requested_model {
        Some(_) => "request",
        None if session_selected_model
            .filter(|model_id| !model_id.trim().is_empty())
            .is_some_and(|model_id| {
                resolve_model_catalogue_entry(&config.models, model_id).is_some()
            }) =>
        {
            "conversation"
        }
        None => "configured_default",
    };
    let config = effective_session_config(config, selected_model_id);
    let workflow_capabilities =
        crate::capabilities::WorkflowCapabilities::for_protocol(config.default_provider_protocol());
    if let Err(refusal) =
        check_attachment_support(&workflow_capabilities, &config.attachments, attachments)
    {
        if let Some(scheme) = &refusal.refused_scheme
            && let Err(error) = state
                .storage
                .create_audit_event(swage_storage::NewAuditEvent {
                    id: Uuid::new_v4().to_string(),
                    session_id: Some(session_id),
                    entity_kind: "session".into(),
                    entity_id: session_id.to_string(),
                    event_kind: "image_reference_refused".into(),
                    summary: refusal.reason.clone(),
                    metadata: serde_json::json!({ "reason": refusal.reason, "scheme": scheme }),
                })
                .await
        {
            tracing::error!(%session_id, %error, "image reference refused but recording the refusal in the audit trail failed");
        }
        return Err(refusal.into());
    }

    Ok((
        config,
        effective_model_source,
        selected_model_id.map(str::to_owned),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capabilities::WorkflowCapabilities;

    #[test]
    fn image_scheme_defaults_refuse_before_provider_checks_and_preserve_accepted_references() {
        let capabilities = WorkflowCapabilities::for_protocol(swage_config::Protocol::OpenAi);
        let policy = swage_config::AttachmentsConfig::default();
        for url in [
            "https://example.test/a.png",
            "data:image/png;base64,cG5n",
            "HTTPS://example.test/a.png",
        ] {
            let attachments = [MessageAttachment::ImageUrl { url: url.into() }];
            check_attachment_support(&capabilities, &policy, &attachments).unwrap();
            assert_eq!(
                content_parts("look".into(), &attachments)[1],
                ContentPart::ImageUrl { url: url.into() }
            );
        }
        for (url, scheme) in [
            ("http://example.test/a.png", "http"),
            ("file:///private/image", "file"),
            ("ftp://example.test/a.png", "ftp"),
            ("relative/image.png", "<missing>"),
            ("", "<missing>"),
            (" https://example.test/a.png", " https"),
        ] {
            // A text-only provider would also refuse; policy gets first say.
            let refusal = check_attachment_support(
                &WorkflowCapabilities::default(),
                &policy,
                &[MessageAttachment::ImageUrl { url: url.into() }],
            )
            .unwrap_err();
            assert_eq!(refusal.refused_scheme.as_deref(), Some(scheme));
            assert!(
                refusal.reason.contains(scheme) && refusal.reason.contains("https, data"),
                "{}",
                refusal.reason
            );
        }
    }

    #[test]
    fn image_scheme_override_changes_admission_without_relaxing_provider_restrictions() {
        let capabilities = WorkflowCapabilities::for_protocol(swage_config::Protocol::OpenAi);
        let policy = swage_config::AttachmentsConfig {
            accepted_schemes: vec!["HTTP".into()],
        };
        let http = [MessageAttachment::ImageUrl {
            url: "http://example.test/a.png".into(),
        }];
        check_attachment_support(&capabilities, &policy, &http).unwrap();
        assert!(
            check_attachment_support(
                &capabilities,
                &policy,
                &[MessageAttachment::ImageUrl {
                    url: "https://example.test/a.png".into()
                }]
            )
            .is_err()
        );
        let google = WorkflowCapabilities::for_protocol(swage_config::Protocol::Google);
        let refusal = check_attachment_support(&google, &policy, &http).unwrap_err();
        assert!(refusal.refused_scheme.is_none());
        assert!(refusal.reason.contains("cannot resolve"));
        let refusal =
            check_attachment_support(&WorkflowCapabilities::default(), &policy, &http).unwrap_err();
        assert!(refusal.reason.contains("does not accept image"));
    }
}

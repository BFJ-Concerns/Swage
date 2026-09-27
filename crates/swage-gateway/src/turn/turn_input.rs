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

use super::{
    AppState, ContentPart, GatewayError, MessageAttachment, Uuid, effective_session_config,
    ensure_selected_model_available, resolve_model_catalogue_entry, resolve_requested_turn_model,
};

/// What a caller submits for one turn: the message text plus the content
/// parts sent beside it. Every turn entry point takes this; a `String`
/// converts, so a text-only caller passes its text unchanged.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TurnInput {
    pub text: String,
    pub attachments: Vec<MessageAttachment>,
}

impl TurnInput {
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            attachments: Vec::new(),
        }
    }

    pub fn with_attachments(text: impl Into<String>, attachments: Vec<MessageAttachment>) -> Self {
        Self {
            text: text.into(),
            attachments,
        }
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

/// Refuse a submission whose attachments the selected provider cannot
/// consume. The check runs before anything is persisted: a part the model
/// would never see must fail the turn, not ride along silently.
pub(crate) fn check_attachment_support(
    capabilities: &crate::capabilities::WorkflowCapabilities,
    attachments: &[MessageAttachment],
) -> Result<(), GatewayError> {
    for attachment in attachments {
        match attachment {
            MessageAttachment::ImageUrl { url } => {
                if url.trim().is_empty() {
                    return Err(GatewayError::ValidationError(
                        "an image attachment needs a non-empty url".into(),
                    ));
                }
                if !capabilities.vision() {
                    return Err(GatewayError::ValidationError(
                        "the selected provider does not accept image attachments".into(),
                    ));
                }
                if !capabilities.accepts_image_reference(url) {
                    return Err(GatewayError::ValidationError(
                        "the selected provider cannot resolve this image reference: attach the \
                         image inline as a base64 data URL"
                            .into(),
                    ));
                }
            }
        }
    }
    Ok(())
}

/// Resolve the request's effective model and admit its attachments against
/// that model's provider protocol. Every entry point uses this seam before it
/// persists a turn, so the admission decision cannot drift from execution.
pub(crate) fn resolve_turn_model_and_attachments(
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
    check_attachment_support(&workflow_capabilities, attachments)?;

    Ok((
        config,
        effective_model_source,
        selected_model_id.map(str::to_owned),
    ))
}

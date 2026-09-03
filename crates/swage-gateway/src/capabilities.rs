use serde::{Deserialize, Serialize};

use crate::state::AppState;

/// The image references an adapter can put on its provider's wire. An
/// inline `data:` URL is carried as bytes by every adapter that maps image
/// parts; the difference between protocols is what they do with a reference
/// to an image held elsewhere.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ImageReferences {
    /// The provider fetches an ordinary web URL itself, so any reference
    /// the client submits reaches the model.
    AnyUrl,
    /// The provider fetches nothing: a reference resolves only when the
    /// provider's own file service issued it, under this URL prefix. Any
    /// other reference is a URI the provider cannot resolve, so it is
    /// refused at admission rather than sent as an unusable reference.
    InlineOrHostedFile { file_service_prefix: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowCapabilities {
    pub text: bool,
    /// The image references the adapter maps, or `None` where it maps no
    /// image parts at all. One value answers both "may a client attach an
    /// image?" and "which references survive the wire", so the vision claim
    /// and the accepted forms cannot drift apart.
    pub images: Option<ImageReferences>,
    pub embeddings: bool,
    pub image_generation: bool,
}

impl Default for WorkflowCapabilities {
    fn default() -> Self {
        Self {
            text: true,
            images: None,
            embeddings: false,
            image_generation: false,
        }
    }
}

impl WorkflowCapabilities {
    /// What the adapter for a protocol actually puts on the wire. An image
    /// claim is made only where the adapter maps `ContentPart::ImageUrl`,
    /// and only for the references that adapter's provider can resolve: the
    /// intake gate refuses everything else rather than letting the adapter
    /// drop it or forward it unusable, so a claim here is a promise the
    /// adapter's message conversion keeps.
    pub fn for_protocol(protocol: swage_config::Protocol) -> Self {
        match protocol {
            // Each adapter maps ContentPart::ImageUrl in its own native
            // message representation; OpenRouter shares Chat Completions.
            // These providers fetch an ordinary image URL themselves.
            swage_config::Protocol::OpenAi
            | swage_config::Protocol::OpenAiResponses
            | swage_config::Protocol::OpenRouter
            | swage_config::Protocol::Anthropic => Self {
                text: true,
                images: Some(ImageReferences::AnyUrl),
                embeddings: false,
                image_generation: false,
            },
            // Gemini inlines a data URL as `inlineData`, but `fileData.fileUri`
            // addresses a resource already uploaded to the Google File API —
            // it is not a fetch instruction for an arbitrary web image.
            swage_config::Protocol::Google => Self {
                text: true,
                images: Some(ImageReferences::InlineOrHostedFile {
                    file_service_prefix: GOOGLE_FILE_SERVICE_PREFIX.to_string(),
                }),
                embeddings: false,
                image_generation: false,
            },
            // The first App Server integration contributes assembled text;
            // attachment projection is a separate wire capability.
            swage_config::Protocol::Codex => Self::default(),
        }
    }

    /// Whether the adapter maps image parts at all.
    pub fn vision(&self) -> bool {
        self.images.is_some()
    }

    /// Whether the adapter can put this particular image reference on the
    /// wire in a form its provider resolves.
    pub fn accepts_image_reference(&self, url: &str) -> bool {
        match &self.images {
            None => false,
            Some(ImageReferences::AnyUrl) => true,
            Some(ImageReferences::InlineOrHostedFile {
                file_service_prefix,
            }) => is_inline_image_data_url(url) || url.starts_with(file_service_prefix.as_str()),
        }
    }
}

/// Where the Google File API serves an uploaded resource. A `fileData.fileUri`
/// outside this prefix is a reference Gemini cannot resolve.
const GOOGLE_FILE_SERVICE_PREFIX: &str = "https://generativelanguage.googleapis.com/";

/// A `data:` URL carrying base64 image bytes inline, which every adapter that
/// maps image parts can embed without the provider fetching anything.
fn is_inline_image_data_url(url: &str) -> bool {
    url.strip_prefix("data:")
        .and_then(|data_url| data_url.split_once(','))
        .and_then(|(metadata, data)| {
            let mime_type = metadata.strip_suffix(";base64")?;
            (!mime_type.is_empty() && !data.is_empty()).then_some(())
        })
        .is_some()
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkflowCapabilitiesResponse {
    pub text: bool,
    pub vision: bool,
    pub embeddings: bool,
    pub image_generation: bool,
    pub tts: bool,
    pub transcription: bool,
}

pub async fn workflow_capabilities(state: &AppState) -> WorkflowCapabilitiesResponse {
    let voice = state.voice_provider.capabilities();
    WorkflowCapabilitiesResponse {
        text: state.workflow_capabilities.text,
        vision: state.workflow_capabilities.vision(),
        embeddings: state.workflow_capabilities.embeddings,
        image_generation: state.workflow_capabilities.image_generation,
        tts: voice.synthesis,
        transcription: voice.transcription,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A vision claim is a promise the adapter keeps: only the protocols
    /// whose adapter maps `ContentPart::ImageUrl` may claim it.
    #[test]
    fn vision_is_claimed_only_where_the_adapter_maps_image_parts() {
        use swage_config::Protocol;
        for protocol in [
            Protocol::OpenAi,
            Protocol::OpenAiResponses,
            Protocol::OpenRouter,
            Protocol::Anthropic,
            Protocol::Google,
        ] {
            assert!(
                WorkflowCapabilities::for_protocol(protocol.clone()).vision(),
                "{protocol:?} maps image parts"
            );
        }
        assert!(
            !WorkflowCapabilities::for_protocol(Protocol::Codex).vision(),
            "Codex has no documented app-server image input"
        );
    }

    /// The claim covers the references the provider can actually resolve.
    /// Gemini fetches nothing: a `fileData.fileUri` outside the Google File
    /// API is a reference the model never resolves, so the claim excludes it.
    #[test]
    fn google_claims_only_the_references_gemini_can_resolve() {
        let google = WorkflowCapabilities::for_protocol(swage_config::Protocol::Google);

        assert!(
            google.accepts_image_reference("data:image/png;base64,cG5nLWJ5dGVz"),
            "an inline data URL travels as bytes"
        );
        assert!(
            google.accepts_image_reference(
                "https://generativelanguage.googleapis.com/v1beta/files/abc123"
            ),
            "a Google File API resource is what fileUri addresses"
        );
        assert!(
            !google.accepts_image_reference("https://example.test/cat.png"),
            "Gemini does not fetch an ordinary web image URL"
        );
        assert!(
            !google.accepts_image_reference("data:image/png,not-base64"),
            "only base64 data URLs can be inlined"
        );
    }

    /// The providers that fetch a URL themselves accept any reference.
    #[test]
    fn url_fetching_protocols_accept_any_image_reference() {
        for protocol in [
            swage_config::Protocol::OpenAi,
            swage_config::Protocol::OpenAiResponses,
            swage_config::Protocol::OpenRouter,
            swage_config::Protocol::Anthropic,
        ] {
            let capabilities = WorkflowCapabilities::for_protocol(protocol.clone());
            assert!(
                capabilities.accepts_image_reference("https://example.test/cat.png"),
                "{protocol:?} fetches an image URL itself"
            );
        }
        assert!(
            !WorkflowCapabilities::for_protocol(swage_config::Protocol::Codex)
                .accepts_image_reference("data:image/png;base64,cG5nLWJ5dGVz"),
            "a protocol that maps no image parts accepts no reference"
        );
    }
}

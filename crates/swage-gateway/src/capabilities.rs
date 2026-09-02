use serde::{Deserialize, Serialize};

use crate::state::AppState;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowCapabilities {
    pub text: bool,
    pub vision: bool,
    pub embeddings: bool,
    pub image_generation: bool,
}

impl Default for WorkflowCapabilities {
    fn default() -> Self {
        Self {
            text: true,
            vision: false,
            embeddings: false,
            image_generation: false,
        }
    }
}

impl WorkflowCapabilities {
    /// What the adapter for a protocol actually puts on the wire. `vision`
    /// is claimed only where the adapter maps `ContentPart::ImageUrl`: the
    /// intake gate refuses image parts on every other protocol rather than
    /// letting the adapter drop them, so a claim here is a promise the
    /// adapter's message conversion keeps.
    pub fn for_protocol(protocol: swage_config::Protocol) -> Self {
        match protocol {
            // Each adapter maps ContentPart::ImageUrl in its own native
            // message representation; OpenRouter shares Chat Completions.
            swage_config::Protocol::OpenAi
            | swage_config::Protocol::OpenAiResponses
            | swage_config::Protocol::OpenRouter
            | swage_config::Protocol::Anthropic
            | swage_config::Protocol::Google => Self {
                text: true,
                vision: true,
                embeddings: false,
                image_generation: false,
            },
            // The first App Server integration contributes assembled text;
            // attachment projection is a separate wire capability.
            swage_config::Protocol::Codex => Self::default(),
        }
    }
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
        vision: state.workflow_capabilities.vision,
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
                WorkflowCapabilities::for_protocol(protocol.clone()).vision,
                "{protocol:?} maps image parts"
            );
        }
        assert!(
            !WorkflowCapabilities::for_protocol(Protocol::Codex).vision,
            "Codex has no documented app-server image input"
        );
    }
}

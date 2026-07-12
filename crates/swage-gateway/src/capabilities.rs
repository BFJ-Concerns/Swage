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
    pub fn for_protocol(protocol: swage_config::Protocol) -> Self {
        match protocol {
            swage_config::Protocol::OpenAi
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

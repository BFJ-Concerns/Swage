//! Gateway voice surface.
//!
//! Translates direct client audio requests into provider-backed transcription
//! and synthesis calls without changing the canonical text turn pipeline.

use serde::{Deserialize, Serialize};
use swage_provider::{
    SpeechRequest, SpeechResponse, TranscriptionRequest, TranscriptionResponse, VoiceCapabilities,
};

use crate::{error::ApiError, state::AppState};
use swage_core::GatewayError;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoiceCapabilitiesResponse {
    pub transcription: bool,
    pub synthesis: bool,
    pub default_voice: Option<String>,
    #[serde(default)]
    pub supported_voices: Vec<String>,
}

impl From<VoiceCapabilities> for VoiceCapabilitiesResponse {
    fn from(value: VoiceCapabilities) -> Self {
        Self {
            transcription: value.transcription,
            synthesis: value.synthesis,
            default_voice: value.default_voice,
            supported_voices: value.supported_voices,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscribeAudioRequest {
    pub audio_bytes: Vec<u8>,
    pub mime_type: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscribeAudioResponse {
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SynthesiseSpeechRequest {
    pub text: String,
    #[serde(default)]
    pub voice: Option<String>,
    #[serde(default)]
    pub format: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SynthesiseSpeechResponse {
    pub audio_bytes: Vec<u8>,
    pub mime_type: String,
    pub voice: String,
}

pub async fn voice_capabilities(state: &AppState) -> VoiceCapabilitiesResponse {
    state.voice_provider.capabilities().into()
}

pub async fn transcribe_audio(
    state: &AppState,
    request: TranscribeAudioRequest,
) -> Result<TranscribeAudioResponse, ApiError> {
    let capabilities = state.voice_provider.capabilities();
    if !capabilities.transcription {
        return Err(ApiError::validation(
            "voice transcription is unavailable for the configured runtime".to_string(),
        ));
    }
    if request.audio_bytes.is_empty() {
        return Err(ApiError::validation(
            "audio_bytes must not be empty".to_string(),
        ));
    }
    if request.mime_type.trim().is_empty() {
        return Err(ApiError::validation(
            "mime_type must not be empty".to_string(),
        ));
    }

    let response: TranscriptionResponse = state
        .voice_provider
        .transcribe(TranscriptionRequest {
            audio_bytes: request.audio_bytes,
            mime_type: request.mime_type,
        })
        .await
        .map_err(|error| ApiError::from(GatewayError::Provider(error)))?;

    Ok(TranscribeAudioResponse {
        text: response.text,
    })
}

pub async fn synthesise_speech(
    state: &AppState,
    request: SynthesiseSpeechRequest,
) -> Result<SynthesiseSpeechResponse, ApiError> {
    let capabilities = state.voice_provider.capabilities();
    if !capabilities.synthesis {
        return Err(ApiError::validation(
            "voice synthesis is unavailable for the configured runtime".to_string(),
        ));
    }
    if request.text.trim().is_empty() {
        return Err(ApiError::validation("text must not be empty".to_string()));
    }

    let response: SpeechResponse = state
        .voice_provider
        .synthesise(SpeechRequest {
            text: request.text,
            voice: request.voice,
            format: request.format,
        })
        .await
        .map_err(|error| ApiError::from(GatewayError::Provider(error)))?;

    Ok(SynthesiseSpeechResponse {
        audio_bytes: response.audio_bytes,
        mime_type: response.mime_type,
        voice: response.voice,
    })
}

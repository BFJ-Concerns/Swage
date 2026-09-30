//! Gateway voice surface.
//!
//! Translates direct client audio requests into provider-backed transcription
//! and synthesis calls without changing the canonical text turn pipeline.

use swage_provider::{SpeechRequest, SpeechResponse, TranscriptionRequest, TranscriptionResponse};

use crate::{error::ApiError, state::AppState};
use swage_core::GatewayError;

pub use swage_protocol::client::voice::{
    SynthesiseSpeechRequest, SynthesiseSpeechResponse, TranscribeAudioRequest,
    TranscribeAudioResponse, VoiceCapabilitiesResponse,
};

pub async fn voice_capabilities(state: &AppState) -> VoiceCapabilitiesResponse {
    let capabilities = state.active_voice_provider().capabilities();
    VoiceCapabilitiesResponse {
        transcription: capabilities.transcription,
        synthesis: capabilities.synthesis,
        default_voice: capabilities.default_voice,
        supported_voices: capabilities.supported_voices,
        unavailable_reason: capabilities.unavailable_reason,
    }
}

pub async fn transcribe_audio(
    state: &AppState,
    request: TranscribeAudioRequest,
) -> Result<TranscribeAudioResponse, ApiError> {
    let voice_provider = state.active_voice_provider();
    let capabilities = voice_provider.capabilities();
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

    let response: TranscriptionResponse = voice_provider
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
    let voice_provider = state.active_voice_provider();
    let capabilities = voice_provider.capabilities();
    if !capabilities.synthesis {
        return Err(ApiError::validation(
            "voice synthesis is unavailable for the configured runtime".to_string(),
        ));
    }
    if request.text.trim().is_empty() {
        return Err(ApiError::validation("text must not be empty".to_string()));
    }

    let response: SpeechResponse = voice_provider
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

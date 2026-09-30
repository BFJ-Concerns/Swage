//! Voice capability and audio request bodies.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct VoiceCapabilitiesResponse {
    pub transcription: bool,
    pub synthesis: bool,
    pub default_voice: Option<String>,
    #[serde(default)]
    #[ts(optional, as = "Option<_>")]
    pub supported_voices: Vec<String>,
    /// Why transcription or synthesis is unavailable, naming what to
    /// configure; absent when both are available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub unavailable_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct TranscribeAudioRequest {
    pub audio_bytes: Vec<u8>,
    pub mime_type: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct TranscribeAudioResponse {
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct SynthesiseSpeechRequest {
    pub text: String,
    #[serde(default)]
    #[ts(optional, as = "Option<_>")]
    pub voice: Option<String>,
    #[serde(default)]
    #[ts(optional, as = "Option<_>")]
    pub format: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct SynthesiseSpeechResponse {
    pub audio_bytes: Vec<u8>,
    pub mime_type: String,
    pub voice: String,
}

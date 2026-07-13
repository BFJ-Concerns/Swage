//! Voice capability and audio request bodies.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
pub struct VoiceCapabilitiesResponse {
    pub transcription: bool,
    pub synthesis: bool,
    pub default_voice: Option<String>,
    #[serde(default)]
    pub supported_voices: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
pub struct TranscribeAudioRequest {
    pub audio_bytes: Vec<u8>,
    pub mime_type: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
pub struct TranscribeAudioResponse {
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
pub struct SynthesiseSpeechRequest {
    pub text: String,
    #[serde(default)]
    pub voice: Option<String>,
    #[serde(default)]
    pub format: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
pub struct SynthesiseSpeechResponse {
    pub audio_bytes: Vec<u8>,
    pub mime_type: String,
    pub voice: String,
}

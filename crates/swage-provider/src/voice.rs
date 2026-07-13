//! Voice provider abstraction for speech-to-text and text-to-speech.
//!
//! Keeps audio support behind one runtime boundary so the gateway and clients
//! do not need provider-specific branching.

use async_trait::async_trait;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue};
use serde::{Deserialize, Serialize};
use swage_core::ProviderError;

use crate::provider::ProviderConfig;

/// Declares which voice features a configured runtime can actually use.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VoiceCapabilities {
    pub transcription: bool,
    pub synthesis: bool,
    pub default_voice: Option<String>,
    #[serde(default)]
    pub supported_voices: Vec<String>,
}

impl VoiceCapabilities {
    pub fn unsupported() -> Self {
        Self {
            transcription: false,
            synthesis: false,
            default_voice: None,
            supported_voices: Vec::new(),
        }
    }
}

/// Request payload for speech-to-text.
#[derive(Debug, Clone)]
pub struct TranscriptionRequest {
    pub audio_bytes: Vec<u8>,
    pub mime_type: String,
}

/// Result of speech-to-text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranscriptionResponse {
    pub text: String,
}

/// Request payload for text-to-speech.
#[derive(Debug, Clone)]
pub struct SpeechRequest {
    pub text: String,
    pub voice: Option<String>,
    pub format: Option<String>,
}

/// Result of text-to-speech.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpeechResponse {
    pub audio_bytes: Vec<u8>,
    pub mime_type: String,
    pub voice: String,
}

#[async_trait]
pub trait VoiceProvider: Send + Sync {
    fn capabilities(&self) -> VoiceCapabilities;

    async fn transcribe(
        &self,
        request: TranscriptionRequest,
    ) -> Result<TranscriptionResponse, ProviderError>;

    async fn synthesise(&self, request: SpeechRequest) -> Result<SpeechResponse, ProviderError>;
}

/// Default runtime when no configured provider exposes voice features.
pub struct UnsupportedVoiceProvider {
    reason: String,
}

impl UnsupportedVoiceProvider {
    pub fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
        }
    }
}

#[async_trait]
impl VoiceProvider for UnsupportedVoiceProvider {
    fn capabilities(&self) -> VoiceCapabilities {
        VoiceCapabilities::unsupported()
    }

    async fn transcribe(
        &self,
        _request: TranscriptionRequest,
    ) -> Result<TranscriptionResponse, ProviderError> {
        Err(ProviderError::InvalidRequest {
            message: self.reason.clone(),
        })
    }

    async fn synthesise(&self, _request: SpeechRequest) -> Result<SpeechResponse, ProviderError> {
        Err(ProviderError::InvalidRequest {
            message: self.reason.clone(),
        })
    }
}

/// OpenAI-compatible voice provider.
///
/// Uses the standard `/audio/transcriptions` and `/audio/speech` endpoints.
pub struct OpenAiVoiceProvider {
    config: ProviderConfig,
}

impl OpenAiVoiceProvider {
    pub fn from_config(config: ProviderConfig) -> Self {
        Self { config }
    }

    fn auth_headers(&self) -> HeaderMap {
        let mut headers = HeaderMap::new();
        if let Some(ref key) = self.config.api_key
            && let Ok(value) = HeaderValue::from_str(&format!("Bearer {}", key.expose_secret()))
        {
            headers.insert(AUTHORIZATION, value);
        }
        headers
    }

    fn supported_voices() -> Vec<String> {
        [
            "alloy", "ash", "ballad", "coral", "echo", "fable", "nova", "onyx", "sage", "shimmer",
        ]
        .into_iter()
        .map(str::to_string)
        .collect()
    }
}

#[derive(Deserialize)]
struct OpenAiTranscriptionResponse {
    text: String,
}

#[async_trait]
impl VoiceProvider for OpenAiVoiceProvider {
    fn capabilities(&self) -> VoiceCapabilities {
        VoiceCapabilities {
            transcription: true,
            synthesis: true,
            default_voice: Some("alloy".to_string()),
            supported_voices: Self::supported_voices(),
        }
    }

    async fn transcribe(
        &self,
        request: TranscriptionRequest,
    ) -> Result<TranscriptionResponse, ProviderError> {
        let file_part = reqwest::multipart::Part::bytes(request.audio_bytes)
            .mime_str(&request.mime_type)
            .map_err(|error| ProviderError::InvalidRequest {
                message: format!("invalid audio mime type: {error}"),
            })?
            .file_name("input-audio");

        let form = reqwest::multipart::Form::new()
            .part("file", file_part)
            .text("model", self.config.model.clone());

        let response = reqwest::Client::new()
            .post(format!(
                "{}/audio/transcriptions",
                self.config.base_url.trim_end_matches('/')
            ))
            .headers(self.auth_headers())
            .multipart(form)
            .send()
            .await
            .map_err(|error| ProviderError::Network {
                provider: "openai-voice".to_string(),
                message: error.to_string(),
            })?;

        let status = response.status();
        if !status.is_success() {
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "<failed to read body>".to_string());
            return Err(ProviderError::HttpError {
                status: status.as_u16(),
                body,
            });
        }

        let parsed: OpenAiTranscriptionResponse = response
            .json()
            .await
            .map_err(|error| ProviderError::InvalidResponse(error.to_string()))?;
        Ok(TranscriptionResponse { text: parsed.text })
    }

    async fn synthesise(&self, request: SpeechRequest) -> Result<SpeechResponse, ProviderError> {
        let voice = request.voice.unwrap_or_else(|| {
            self.capabilities()
                .default_voice
                .unwrap_or_else(|| "alloy".into())
        });
        let format = request.format.unwrap_or_else(|| "wav".to_string());
        let mime_type = match format.as_str() {
            "mp3" => "audio/mpeg",
            "flac" => "audio/flac",
            "opus" => "audio/ogg",
            _ => "audio/wav",
        }
        .to_string();

        let response = reqwest::Client::new()
            .post(format!(
                "{}/audio/speech",
                self.config.base_url.trim_end_matches('/')
            ))
            .headers({
                let mut headers = self.auth_headers();
                headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
                headers
            })
            .json(&serde_json::json!({
                "model": self.config.model,
                "input": request.text,
                "voice": voice,
                "response_format": format,
            }))
            .send()
            .await
            .map_err(|error| ProviderError::Network {
                provider: "openai-voice".to_string(),
                message: error.to_string(),
            })?;

        let status = response.status();
        if !status.is_success() {
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "<failed to read body>".to_string());
            return Err(ProviderError::HttpError {
                status: status.as_u16(),
                body,
            });
        }

        let bytes = response
            .bytes()
            .await
            .map_err(|error| ProviderError::InvalidResponse(error.to_string()))?;

        Ok(SpeechResponse {
            audio_bytes: bytes.to_vec(),
            mime_type,
            voice,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsupported_capabilities_are_empty() {
        let provider = UnsupportedVoiceProvider::new("voice is unavailable");
        let capabilities = provider.capabilities();
        assert!(!capabilities.transcription);
        assert!(!capabilities.synthesis);
        assert!(capabilities.supported_voices.is_empty());
    }

    #[test]
    fn openai_voice_provider_reports_expected_defaults() {
        let provider = OpenAiVoiceProvider::from_config(ProviderConfig::new(
            "key",
            "gpt-4o-mini-tts",
            "https://api.openai.com/v1",
        ));
        let capabilities = provider.capabilities();
        assert!(capabilities.transcription);
        assert!(capabilities.synthesis);
        assert_eq!(capabilities.default_voice.as_deref(), Some("alloy"));
        assert!(
            capabilities
                .supported_voices
                .iter()
                .any(|voice| voice == "nova")
        );
    }
}

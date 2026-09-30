//! Voice provider abstraction for speech-to-text and text-to-speech.
//!
//! Keeps audio support behind one runtime boundary so the gateway and clients
//! do not need provider-specific branching.

use async_trait::async_trait;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue};
use serde::{Deserialize, Serialize};
use swage_core::ProviderError;

use swage_config::MaskedString;

use crate::provider::ProviderConfig;

/// Declares which voice features a configured runtime can actually use.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VoiceCapabilities {
    pub transcription: bool,
    pub synthesis: bool,
    pub default_voice: Option<String>,
    #[serde(default)]
    pub supported_voices: Vec<String>,
    /// Why transcription or synthesis is unavailable, naming what to
    /// configure; `None` when both are available.
    #[serde(default)]
    pub unavailable_reason: Option<String>,
}

impl VoiceCapabilities {
    pub fn unsupported(reason: impl Into<String>) -> Self {
        Self {
            transcription: false,
            synthesis: false,
            default_voice: None,
            supported_voices: Vec::new(),
            unavailable_reason: Some(reason.into()),
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
        VoiceCapabilities::unsupported(self.reason.clone())
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

/// The models an endpoint serves for each audio API. Voice never borrows the
/// chat model: an unset id leaves that capability unavailable.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VoiceModelRoutes {
    pub transcription_model: Option<String>,
    pub speech_model: Option<String>,
}

/// OpenAI-compatible voice provider.
///
/// Uses the standard `/audio/transcriptions` and `/audio/speech` endpoints,
/// requesting each with the model its route names.
pub struct OpenAiVoiceProvider {
    api_key: Option<MaskedString>,
    base_url: String,
    routes: VoiceModelRoutes,
}

impl OpenAiVoiceProvider {
    /// Builds the provider from an endpoint's credentials and base URL plus
    /// the audio model routes. The endpoint config's chat model is not used.
    pub fn new(endpoint: ProviderConfig, routes: VoiceModelRoutes) -> Self {
        Self {
            api_key: endpoint.api_key,
            base_url: endpoint.base_url,
            routes,
        }
    }

    fn unavailable_reason(&self) -> Option<String> {
        let unset: Vec<&str> = [
            ("transcription_model", &self.routes.transcription_model),
            ("speech_model", &self.routes.speech_model),
        ]
        .into_iter()
        .filter(|(_, model)| model.is_none())
        .map(|(key, _)| key)
        .collect();
        if unset.is_empty() {
            return None;
        }
        Some(format!(
            "set {} in the [voice] settings to a model the default provider serves",
            unset
                .iter()
                .map(|key| format!("`{key}`"))
                .collect::<Vec<_>>()
                .join(" and ")
        ))
    }

    fn route_model<'a>(&self, model: &'a Option<String>) -> Result<&'a str, ProviderError> {
        model
            .as_deref()
            .ok_or_else(|| ProviderError::InvalidRequest {
                message: self
                    .unavailable_reason()
                    .unwrap_or_else(|| "voice model is not configured".to_string()),
            })
    }

    fn auth_headers(&self) -> HeaderMap {
        let mut headers = HeaderMap::new();
        if let Some(ref key) = self.api_key
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
            transcription: self.routes.transcription_model.is_some(),
            synthesis: self.routes.speech_model.is_some(),
            default_voice: Some("alloy".to_string()),
            supported_voices: Self::supported_voices(),
            unavailable_reason: self.unavailable_reason(),
        }
    }

    async fn transcribe(
        &self,
        request: TranscriptionRequest,
    ) -> Result<TranscriptionResponse, ProviderError> {
        let model = self.route_model(&self.routes.transcription_model)?;
        let file_part = reqwest::multipart::Part::bytes(request.audio_bytes)
            .mime_str(&request.mime_type)
            .map_err(|error| ProviderError::InvalidRequest {
                message: format!("invalid audio mime type: {error}"),
            })?
            .file_name("input-audio");

        let form = reqwest::multipart::Form::new()
            .part("file", file_part)
            .text("model", model.to_string());

        let response = reqwest::Client::new()
            .post(format!(
                "{}/audio/transcriptions",
                self.base_url.trim_end_matches('/')
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
        let model = self.route_model(&self.routes.speech_model)?;
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
                self.base_url.trim_end_matches('/')
            ))
            .headers({
                let mut headers = self.auth_headers();
                headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
                headers
            })
            .json(&serde_json::json!({
                "model": model,
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

    /// The chat model the endpoint config carries; no audio request may use it.
    const CHAT_MODEL: &str = "chat-model-not-for-audio";

    fn routed(base_url: &str, routes: VoiceModelRoutes) -> OpenAiVoiceProvider {
        OpenAiVoiceProvider::new(ProviderConfig::new("key", CHAT_MODEL, base_url), routes)
    }

    fn both_routes() -> VoiceModelRoutes {
        VoiceModelRoutes {
            transcription_model: Some("whisper-route".into()),
            speech_model: Some("speech-route".into()),
        }
    }

    /// Accepts one HTTP request, answers it with `body`, and returns the raw
    /// request bytes as text.
    async fn capture_one_request(body: &'static str) -> (String, tokio::task::JoinHandle<String>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let handle = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut received = Vec::new();
            let mut buffer = [0u8; 4096];
            loop {
                let read = socket.read(&mut buffer).await.unwrap();
                received.extend_from_slice(&buffer[..read]);
                let text = String::from_utf8_lossy(&received);
                if let Some(header_end) = text.find("\r\n\r\n") {
                    let length = text[..header_end]
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|value| value.trim().parse::<usize>().unwrap())
                        })
                        .unwrap_or(0);
                    if received.len() >= header_end + 4 + length {
                        break;
                    }
                }
                if read == 0 {
                    break;
                }
            }
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            socket.write_all(response.as_bytes()).await.unwrap();
            String::from_utf8_lossy(&received).into_owned()
        });
        (base_url, handle)
    }

    #[tokio::test]
    async fn transcription_requests_the_configured_transcription_model() {
        let (base_url, captured) = capture_one_request(r#"{"text":"hello"}"#).await;
        let response = routed(&base_url, both_routes())
            .transcribe(TranscriptionRequest {
                audio_bytes: b"RIFF".to_vec(),
                mime_type: "audio/wav".into(),
            })
            .await
            .unwrap();
        assert_eq!(response.text, "hello");

        let request = captured.await.unwrap();
        assert!(request.starts_with("POST /audio/transcriptions "));
        assert!(
            request.contains("name=\"model\"\r\n\r\nwhisper-route\r\n"),
            "multipart model field must carry the transcription route: {request}"
        );
        assert!(!request.contains(CHAT_MODEL), "{request}");
    }

    #[tokio::test]
    async fn synthesis_requests_the_configured_speech_model() {
        let (base_url, captured) = capture_one_request("RIFFAUDIO").await;
        routed(&base_url, both_routes())
            .synthesise(SpeechRequest {
                text: "hi".into(),
                voice: None,
                format: None,
            })
            .await
            .unwrap();

        let request = captured.await.unwrap();
        assert!(request.starts_with("POST /audio/speech "));
        let body: serde_json::Value =
            serde_json::from_str(request.split("\r\n\r\n").nth(1).unwrap()).unwrap();
        assert_eq!(body["model"], "speech-route");
        assert!(!request.contains(CHAT_MODEL), "{request}");
    }

    #[tokio::test]
    async fn an_unset_route_is_unavailable_and_names_the_setting() {
        let provider = routed(
            "http://127.0.0.1:9",
            VoiceModelRoutes {
                transcription_model: Some("whisper-route".into()),
                speech_model: None,
            },
        );
        let capabilities = provider.capabilities();
        assert!(capabilities.transcription);
        assert!(!capabilities.synthesis);
        let reason = capabilities.unavailable_reason.unwrap();
        assert!(
            reason.contains("speech_model") && reason.contains("[voice]"),
            "{reason}"
        );
        assert!(!reason.contains("transcription_model"), "{reason}");

        let error = provider
            .synthesise(SpeechRequest {
                text: "hi".into(),
                voice: None,
                format: None,
            })
            .await
            .unwrap_err();
        assert!(error.to_string().contains("speech_model"), "{error}");
    }

    #[test]
    fn openai_voice_provider_reports_expected_defaults() {
        let provider = routed("https://api.openai.com/v1", both_routes());
        let capabilities = provider.capabilities();
        assert!(capabilities.transcription);
        assert!(capabilities.synthesis);
        assert_eq!(capabilities.unavailable_reason, None);
        assert_eq!(capabilities.default_voice.as_deref(), Some("alloy"));
        assert!(
            capabilities
                .supported_voices
                .iter()
                .any(|voice| voice == "nova")
        );
    }
}

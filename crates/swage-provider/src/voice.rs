//! Voice provider abstraction for speech-to-text and text-to-speech.
//!
//! Keeps audio support behind one runtime boundary so the gateway and clients
//! do not need provider-specific branching.

use std::sync::RwLock;

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
    /// Whether the configured speech model's availability is still unverified.
    pub synthesis_unverified: bool,
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
            synthesis_unverified: false,
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
    speech_status: RwLock<SpeechStatus>,
}

enum SpeechStatus {
    Unverified,
    Available,
    Failed(String),
}

impl OpenAiVoiceProvider {
    /// Builds the provider from an endpoint's credentials and base URL plus
    /// the audio model routes. The endpoint config's chat model is not used.
    pub fn new(endpoint: ProviderConfig, routes: VoiceModelRoutes) -> Self {
        Self {
            api_key: endpoint.api_key,
            base_url: endpoint.base_url,
            routes,
            speech_status: RwLock::new(SpeechStatus::Unverified),
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

    async fn request_speech(
        &self,
        request: SpeechRequest,
    ) -> Result<SpeechResponse, ProviderError> {
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
                .map_err(|error| ProviderError::InvalidResponse(error.to_string()))?;
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

    fn speech_failure_affects_route(error: &ProviderError) -> bool {
        match error {
            ProviderError::Network { .. } | ProviderError::InvalidResponse(_) => true,
            ProviderError::HttpError {
                status: 401 | 403, ..
            } => true,
            ProviderError::HttpError { body, .. } => {
                let parsed = serde_json::from_str::<serde_json::Value>(body).ok();
                let field = |name| {
                    parsed
                        .as_ref()
                        .and_then(|value| value.pointer(name))
                        .and_then(serde_json::Value::as_str)
                };
                // Model codes identify the failure even when param names a caller input.
                // Message prose cannot reliably identify which resource failed.
                matches!(
                    field("/error/code"),
                    Some(
                        "model_not_found"
                            | "invalid_model"
                            | "unsupported_model"
                            | "model_not_supported"
                            | "unknown_model"
                    )
                ) || field("/error/param") == Some("model")
            }
            _ => false,
        }
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
        let speech_status = self.speech_status.read().unwrap_or_else(|e| e.into_inner());
        VoiceCapabilities {
            transcription: self.routes.transcription_model.is_some(),
            synthesis: self.routes.speech_model.is_some()
                && matches!(*speech_status, SpeechStatus::Available),
            synthesis_unverified: self.routes.speech_model.is_some()
                && matches!(*speech_status, SpeechStatus::Unverified),
            default_voice: Some("alloy".to_string()),
            supported_voices: Self::supported_voices(),
            unavailable_reason: match (&self.routes.speech_model, &*speech_status) {
                (Some(model), SpeechStatus::Unverified) => Some(format!(
                    "speech model `{model}` has not been checked since configuration or its last failed request. Check the Speech model in Voice settings"
                )),
                (_, SpeechStatus::Failed(reason)) => Some(reason.clone()),
                _ => self.unavailable_reason(),
            },
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
        let result = self.request_speech(request).await;
        let mut status = self
            .speech_status
            .write()
            .unwrap_or_else(|e| e.into_inner());
        match &result {
            Ok(_) => *status = SpeechStatus::Available,
            Err(error) if Self::speech_failure_affects_route(error) => {
                *status = SpeechStatus::Failed(format!(
                    "speech model `{}` failed: {error}. Check the Speech model in Voice settings and try again",
                    self.routes.speech_model.as_deref().unwrap_or("not set")
                ));
            }
            // Any failed request invalidates an earlier success, but only route/model
            // evidence can replace a stored failure or turn unchecked into failed.
            Err(_) if matches!(*status, SpeechStatus::Available) => {
                *status = SpeechStatus::Unverified;
            }
            Err(_) => {}
        }
        result
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

    /// Serve scripted responses and capture each complete HTTP request.
    async fn capture_requests(
        responses: Vec<(u16, &'static str)>,
    ) -> (String, tokio::task::JoinHandle<Vec<String>>) {
        capture_http_responses(responses.into_iter().map(|(status, body)| {
            format!(
                "HTTP/1.1 {status} Response\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            )
        }).collect()).await
    }

    async fn capture_http_responses(
        responses: Vec<String>,
    ) -> (String, tokio::task::JoinHandle<Vec<String>>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let handle = tokio::spawn(async move {
            let mut requests = Vec::new();
            for response in responses {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut received = Vec::new();
                let mut buffer = [0u8; 4096];
                loop {
                    let read = socket.read(&mut buffer).await.unwrap();
                    assert!(
                        read > 0,
                        "request must be complete before the socket closes"
                    );
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
                }
                socket.write_all(response.as_bytes()).await.unwrap();
                requests.push(String::from_utf8(received).unwrap());
            }
            requests
        });
        (base_url, handle)
    }

    #[tokio::test]
    async fn transcription_requests_the_configured_transcription_model() {
        let (base_url, captured) = capture_requests(vec![(200, r#"{"text":"hello"}"#)]).await;
        let response = routed(&base_url, both_routes())
            .transcribe(TranscriptionRequest {
                audio_bytes: b"RIFF".to_vec(),
                mime_type: "audio/wav".into(),
            })
            .await
            .unwrap();
        assert_eq!(response.text, "hello");

        let request = captured.await.unwrap().pop().unwrap();
        assert!(request.starts_with("POST /audio/transcriptions "));
        assert!(
            request.contains("name=\"model\"\r\n\r\nwhisper-route\r\n"),
            "multipart model field must carry the transcription route: {request}"
        );
        assert!(!request.contains(CHAT_MODEL), "{request}");
    }

    #[tokio::test]
    async fn synthesis_requests_the_configured_speech_model() {
        let (base_url, captured) = capture_requests(vec![(200, "RIFFAUDIO")]).await;
        routed(&base_url, both_routes())
            .synthesise(SpeechRequest {
                text: "hi".into(),
                voice: None,
                format: None,
            })
            .await
            .unwrap();

        let request = captured.await.unwrap().pop().unwrap();
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
    fn openai_voice_provider_reports_speech_as_unverified_until_requested() {
        let provider = routed("https://api.openai.com/v1", both_routes());
        let capabilities = provider.capabilities();
        assert!(capabilities.transcription);
        assert!(!capabilities.synthesis);
        assert!(
            capabilities
                .unavailable_reason
                .as_deref()
                .unwrap()
                .contains("has not been checked since configuration or its last failed request")
        );
        assert_eq!(capabilities.default_voice.as_deref(), Some("alloy"));
        assert!(
            capabilities
                .supported_voices
                .iter()
                .any(|voice| voice == "nova")
        );
    }
    fn speech_request() -> SpeechRequest {
        SpeechRequest {
            text: "hello".into(),
            voice: None,
            format: None,
        }
    }

    #[tokio::test]
    async fn speech_failure_status_table() {
        // Each response is exercised from all three states, then followed by success.
        let cases = [
            (
                404,
                r#"{"error":{"param":"model","code":"model_not_found","message":"Unsupported voice model 'speech-route'"}}"#,
                true,
            ),
            (
                404,
                r#"{"error":{"param":"voice","code":"model_not_found","message":"Model 'speech-route' does not exist"}}"#,
                true,
            ),
            (
                400,
                r#"{"error":{"param":"format","code":"unsupported_model","message":"Request rejected"}}"#,
                true,
            ),
            (
                500,
                r#"{"error":{"param":"input","code":"invalid_model","message":"Request rejected"}}"#,
                true,
            ),
            (400, r#"{"error":{"code":"model_not_supported"}}"#, true),
            (400, r#"{"error":{"code":"unknown_model"}}"#, true),
            (
                400,
                r#"{"error":{"param":"model","message":"Voice 'invalid' does not exist"}}"#,
                true,
            ),
            (
                400,
                r#"{"error":{"code":"model_not_found","message":"Voice 'invalid' does not exist"}}"#,
                true,
            ),
            (
                429,
                r#"{"error":{"code":"unsupported_model","message":"Unsupported voice 'invalid'"}}"#,
                true,
            ),
            (
                400,
                "Voice 'invalid' is not supported; model 'speech-route' does not exist",
                false,
            ),
            (400, "Model speech-route does not exist", false),
            (
                400,
                "Model 'speech-route' does not exist for voice 'invalid'",
                false,
            ),
            (404, "Unsupported voice model 'speech-route'", false),
            (
                400,
                "Voice 'invalid' is not supported by model 'speech-route'",
                false,
            ),
            (400, "Voice 'invalid' does not exist", false),
            (404, "Voice 'invalid' does not exist", false),
            (500, "Voice 'invalid' does not exist", false),
            (404, "Voice example does not exist", false),
            (
                500,
                r#"{"error":{"param":"voice","message":"Voice 'invalid' could not be loaded"}}"#,
                false,
            ),
            (
                500,
                r#"{"error":{"param":"voice","message":"Request rejected"}}"#,
                false,
            ),
            (
                503,
                r#"{"error":{"param":"voice","message":"Model 'speech-route' does not exist"}}"#,
                false,
            ),
            (
                500,
                r#"{"error":{"code":"invalid_voice","message":"Request rejected"}}"#,
                false,
            ),
            (
                404,
                r#"{"error":{"code":"unsupported_format","message":"Request rejected"}}"#,
                false,
            ),
            (
                422,
                r#"{"error":{"param":"response_format","message":"Unsupported format"}}"#,
                false,
            ),
            (400, "Unsupported format", false),
            (401, "Authentication failed", true),
            (403, "Access denied", true),
            (
                401,
                r#"{"error":{"param":"format","message":"Request rejected"}}"#,
                true,
            ),
            (
                403,
                r#"{"error":{"param":"input","message":"Request rejected"}}"#,
                true,
            ),
            (404, "Route not found", false),
            (410, "Route removed", false),
            (503, "Route unavailable", false),
            (
                429,
                r#"{"error":{"code":"rate_limit_exceeded","message":"Rate limit exceeded"}}"#,
                false,
            ),
            (400, "Request rejected", false),
            (400, "{not-json", false),
            (404, "{not-json", false),
            (500, "{not-json", false),
            (
                400,
                r#"{"message":"model_not_found","code":"invalid_model","param":"model"}"#,
                false,
            ),
            (
                400,
                r#"{"error":{"code":404,"param":null,"message":"model_not_found"}}"#,
                false,
            ),
        ];
        let mut mismatches = Vec::new();
        for (status, body, model_or_route_failure) in cases {
            for before in ["unchecked", "available", "failed"] {
                let mut responses = match before {
                    "available" => vec![(200, "audio")],
                    "failed" => vec![(401, "original authentication failure")],
                    _ => Vec::new(),
                };
                responses.extend([(status, body), (200, "default audio")]);
                let request_count = responses.len();
                let (url, captured) = capture_requests(responses).await;
                let provider = routed(&url, both_routes());
                let unchecked = provider.capabilities();
                match before {
                    "available" => {
                        provider.synthesise(speech_request()).await.unwrap();
                    }
                    "failed" => {
                        provider.synthesise(speech_request()).await.unwrap_err();
                    }
                    _ => {}
                }
                let previous = provider.capabilities();
                let error = provider
                    .synthesise(SpeechRequest {
                        text: "caller input".into(),
                        voice: Some("invalid".into()),
                        format: Some("invalid".into()),
                    })
                    .await
                    .unwrap_err();
                assert!(error.to_string().contains(body));
                let after = provider.capabilities();
                let expected = if model_or_route_failure || before == "failed" {
                    "failed"
                } else {
                    "unchecked"
                };
                let actual = if after.synthesis {
                    "available"
                } else if after.synthesis_unverified {
                    "unchecked"
                } else {
                    "failed"
                };
                if actual != expected {
                    mismatches.push(format!(
                        "{before} + HTTP {status} {body}: expected {expected}, got {actual}"
                    ));
                }
                if model_or_route_failure {
                    if !after
                        .unavailable_reason
                        .as_deref()
                        .is_some_and(|reason| reason.contains(body))
                    {
                        mismatches.push(format!(
                            "{before} + HTTP {status} {body}: stored failure reason missing"
                        ));
                    }
                } else if expected == "failed" {
                    if after.unavailable_reason != previous.unavailable_reason {
                        mismatches.push(format!(
                            "{before} + HTTP {status} {body}: stored failure reason changed"
                        ));
                    }
                } else if after.unavailable_reason != unchecked.unavailable_reason {
                    mismatches.push(format!(
                        "{before} + HTTP {status} {body}: unchecked reason changed"
                    ));
                }
                // Inspect the failed request's state before success can hide a mismatch.
                provider.synthesise(speech_request()).await.unwrap();
                let recovered = provider.capabilities();
                assert!(recovered.synthesis);
                assert!(!recovered.synthesis_unverified);
                assert_eq!(recovered.unavailable_reason, None);
                assert_eq!(captured.await.unwrap().len(), request_count);
            }
        }
        assert!(
            mismatches.is_empty(),
            "speech status mismatches:\n{}",
            mismatches.join("\n")
        );
    }

    #[tokio::test]
    async fn unreadable_speech_responses_fail_shared_status_and_allow_recovery() {
        for status in [200, 400, 404, 500] {
            let (url, captured) = capture_http_responses(vec![
                "HTTP/1.1 200 OK\r\ncontent-length: 5\r\nconnection: close\r\n\r\naudio".into(),
                format!("HTTP/1.1 {status} Response\r\ncontent-length: 100\r\nconnection: close\r\n\r\nshort"),
                "HTTP/1.1 200 OK\r\ncontent-length: 5\r\nconnection: close\r\n\r\naudio".into(),
            ]).await;
            let provider = routed(&url, both_routes());
            provider.synthesise(speech_request()).await.unwrap();
            let error = provider.synthesise(speech_request()).await.unwrap_err();
            assert!(
                matches!(error, ProviderError::InvalidResponse(_)),
                "HTTP {status}: {error}"
            );
            let capabilities = provider.capabilities();
            assert!(!capabilities.synthesis);
            assert!(!capabilities.synthesis_unverified);
            assert!(
                capabilities
                    .unavailable_reason
                    .unwrap()
                    .contains(&error.to_string())
            );
            provider.synthesise(speech_request()).await.unwrap();
            assert!(provider.capabilities().synthesis);
            assert_eq!(provider.capabilities().unavailable_reason, None);
            assert_eq!(captured.await.unwrap().len(), 3);
        }
    }

    #[tokio::test]
    async fn unreachable_speech_route_fails_shared_status() {
        for before in [None, Some((200, "audio")), Some((401, "original failure"))] {
            let (url, stopped) = capture_requests(before.into_iter().collect()).await;
            let provider = routed(&url, both_routes());
            if let Some((status, _)) = before {
                assert_eq!(
                    provider.synthesise(speech_request()).await.is_ok(),
                    status == 200
                );
            }
            stopped.await.unwrap();
            let error = provider.synthesise(speech_request()).await.unwrap_err();
            assert!(matches!(error, ProviderError::Network { .. }));
            let capabilities = provider.capabilities();
            assert!(!capabilities.synthesis);
            assert!(!capabilities.synthesis_unverified);
            assert!(
                capabilities
                    .unavailable_reason
                    .unwrap()
                    .contains(&error.to_string())
            );
        }
    }
}

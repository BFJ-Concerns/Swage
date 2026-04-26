//! Handler for the `swage run` subcommand.
//!
//! Creates a fresh conversation, streams one prompt through the gateway, and
//! writes the assistant's live response to stdout.

use std::io::{self, Write};
use std::path::Path;

use swage_client::error::ClientError;
use swage_client::http::GatewayClient;
use swage_client::websocket::{ClientEvent, GatewayWebSocket, ServerEvent};

use super::RunArgs;

/// Resolve the gateway URL from `--url`, runtime metadata, or the default.
pub fn resolve_gateway_url(args: &RunArgs) -> String {
    match &args.url {
        Some(url) => url.clone(),
        None => match GatewayClient::from_runtime() {
            Ok(client) => client.base_url().to_string(),
            Err(_) => "http://127.0.0.1:3000".to_string(),
        },
    }
}

async fn read_message(args: &RunArgs) -> Result<String, ClientError> {
    if let Some(audio_input) = &args.audio_input {
        let audio_bytes = tokio::fs::read(audio_input)
            .await
            .map_err(|source| ClientError::MetadataIo { source })?;
        let client = GatewayClient::new(&resolve_gateway_url(args));
        let capabilities = client.voice_capabilities().await?;
        if !capabilities.transcription {
            return Err(ClientError::GatewayNotRunning {
                reason: "gateway runtime does not support voice transcription".to_string(),
            });
        }
        let response = client
            .transcribe_audio(audio_bytes, &args.audio_mime_type)
            .await?;
        let trimmed = response.text.trim();
        if trimmed.is_empty() {
            return Err(ClientError::GatewayNotRunning {
                reason: "voice transcription returned empty text".to_string(),
            });
        }
        return Ok(trimmed.to_string());
    }

    if let Some(message) = &args.message {
        let trimmed = message.trim();
        if !trimmed.is_empty() {
            return Ok(trimmed.to_string());
        }
    }

    let stdin_bytes = tokio::task::spawn_blocking(|| {
        let mut buffer = String::new();
        io::stdin().read_line(&mut buffer)?;
        Ok::<_, io::Error>(buffer)
    })
    .await
    .map_err(|join_error| ClientError::GatewayNotRunning {
        reason: format!("failed to read stdin: {join_error}"),
    })?
    .map_err(|source| ClientError::MetadataIo { source })?;

    let trimmed = stdin_bytes.trim();
    if trimmed.is_empty() {
        return Err(ClientError::GatewayNotRunning {
            reason: "run command requires a prompt argument or non-empty stdin".to_string(),
        });
    }

    Ok(trimmed.to_string())
}

/// Run one prompt through the gateway and stream the reply to stdout.
pub async fn run(args: &RunArgs) -> Result<(), ClientError> {
    let url = resolve_gateway_url(args);
    let message = read_message(args).await?;
    let client = GatewayClient::new(&url);
    let conversation = client.create_conversation().await?;
    let mut ws = GatewayWebSocket::connect(&url, conversation.id).await?;

    ws.send(ClientEvent::SendMessage {
        content: message,
        thread_id: None,
    })
    .await?;

    let mut stdout = io::stdout();
    let mut saw_output = false;
    let mut full_output = String::new();

    loop {
        match ws.next_event().await? {
            Some(ServerEvent::Token { content }) => {
                saw_output = true;
                full_output.push_str(&content);
                print!("{content}");
                stdout
                    .flush()
                    .map_err(|source| ClientError::MetadataIo { source })?;
            }
            Some(ServerEvent::TurnComplete { .. }) => {
                if saw_output {
                    println!();
                }
                write_audio_output(args, &client, &full_output).await?;
                stdout
                    .flush()
                    .map_err(|source| ClientError::MetadataIo { source })?;
                return Ok(());
            }
            Some(ServerEvent::Error { message, .. }) => {
                return Err(ClientError::GatewayNotRunning { reason: message });
            }
            Some(ServerEvent::Cancelled {}) => {
                return Err(ClientError::GatewayNotRunning {
                    reason: "turn was cancelled before completion".to_string(),
                });
            }
            Some(ServerEvent::Shutdown { reason }) => {
                return Err(ClientError::GatewayNotRunning {
                    reason: format!("gateway shut down before the run completed: {reason}"),
                });
            }
            Some(_) => {}
            None => {
                return Err(ClientError::WebSocketFailed {
                    address: url,
                    message: "gateway closed the stream before the run completed".to_string(),
                });
            }
        }
    }
}

async fn write_audio_output(
    args: &RunArgs,
    client: &GatewayClient,
    assistant_text: &str,
) -> Result<(), ClientError> {
    let Some(output_path) = &args.audio_output else {
        return Ok(());
    };
    if assistant_text.trim().is_empty() {
        return Err(ClientError::GatewayNotRunning {
            reason: "cannot synthesise empty assistant output".to_string(),
        });
    }

    let capabilities = client.voice_capabilities().await?;
    if !capabilities.synthesis {
        return Err(ClientError::GatewayNotRunning {
            reason: "gateway runtime does not support voice synthesis".to_string(),
        });
    }

    let response = client
        .synthesise_speech(
            assistant_text,
            args.voice.as_deref(),
            Some(&args.audio_format),
        )
        .await?;

    if let Some(parent) = Path::new(output_path).parent()
        && !parent.as_os_str().is_empty()
    {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|source| ClientError::MetadataIo { source })?;
    }

    tokio::fs::write(output_path, response.audio_bytes)
        .await
        .map_err(|source| ClientError::MetadataIo { source })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_url_takes_precedence() {
        let args = RunArgs {
            url: Some("http://example.invalid:1234".into()),
            audio_input: None,
            audio_output: None,
            audio_mime_type: "audio/wav".into(),
            voice: None,
            audio_format: "wav".into(),
            message: Some("hello".into()),
        };

        assert_eq!(resolve_gateway_url(&args), "http://example.invalid:1234");
    }
}

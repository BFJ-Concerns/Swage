//! Handler for the `swage chat` subcommand.
//!
//! Resolves the gateway URL from the `--url` flag, runtime metadata, or a
//! sensible default, then launches the terminal user interface.

use super::TuiArgs;
use swage_client::error::ClientError;

/// Resolve the gateway URL from the `--url` flag, runtime metadata, or the
/// fallback default.
///
/// Resolution order: explicit `--url` flag > running gateway's runtime
/// metadata > `http://127.0.0.1:3000`.
pub fn resolve_gateway_url(args: &TuiArgs) -> String {
    match &args.url {
        Some(url) => url.clone(),
        None => match swage_client::http::GatewayClient::from_runtime() {
            Ok(client) => client.base_url().to_string(),
            // No gateway metadata found — fall back to the conventional default.
            Err(_) => "http://127.0.0.1:3000".to_string(),
        },
    }
}

/// Run the TUI, connecting to the gateway at the resolved URL.
pub async fn run(args: &TuiArgs) -> Result<(), ClientError> {
    let url = resolve_gateway_url(args);
    // Error type is Box<dyn std::error::Error>; map to a ClientError so the
    // combined binary's exit-code handler can surface it uniformly.
    swage_tui::run(&url)
        .await
        .map_err(|e| ClientError::GatewayNotRunning {
            reason: e.to_string(),
        })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::TuiArgs;

    // Explicit --url flag must take precedence over discovery and defaults.
    #[test]
    fn explicit_url_takes_precedence_over_discovery() {
        let args = TuiArgs {
            url: Some("http://explicit:9999".to_string()),
        };
        assert_eq!(resolve_gateway_url(&args), "http://explicit:9999");
    }

    // When no URL is given and no gateway is running, falls back to the default.
    #[test]
    fn falls_back_to_http_address_when_no_url_given() {
        // No running gateway in test env — runtime metadata lookup will fail.
        let args = TuiArgs { url: None };
        let url = resolve_gateway_url(&args);
        assert!(
            url.starts_with("http"),
            "URL should be an HTTP address, got: {url}"
        );
    }
}

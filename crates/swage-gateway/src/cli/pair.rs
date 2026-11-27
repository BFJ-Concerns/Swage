//! `swage gateway pair` — issue a one-time code that pairs a device on the
//! network with the running gateway.
//!
//! A gateway bound beyond loopback admits paired devices only. Pairing codes
//! are issued to loopback requests alone, so this command runs on the
//! gateway's own machine: it finds the running gateway, asks it over loopback
//! for a code, and prints the code for the person to type into the device's
//! pairing prompt before it expires.

use swage_client::{error::ClientError, http::GatewayClient, runtime::RuntimeMetadata};

/// Run the `swage gateway pair` command.
///
/// Exit semantics:
/// - `Ok(())` (exit 0): a code was issued and printed
/// - `Err(_)` (exit non-zero): no running gateway, or the gateway refused
pub async fn run() -> Result<(), ClientError> {
    let installed = super::install::load_install_config()?;
    let meta = match RuntimeMetadata::load() {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            match super::recovery::recover_configured_gateway_metadata(
                installed.as_ref().and_then(|config| config.host.as_deref()),
                installed.as_ref().and_then(|config| config.port),
            )
            .await
            {
                Some(metadata) => metadata,
                None => {
                    return Err(ClientError::GatewayNotRunning {
                        reason: "no running gateway was found to pair with".to_string(),
                    });
                }
            }
        }
        Err(e) => return Err(ClientError::MetadataIo { source: e }),
    };
    if meta.is_stale() {
        let _ = RuntimeMetadata::remove();
        return Err(ClientError::GatewayNotRunning {
            reason: format!("PID {} is no longer running", meta.pid),
        });
    }

    // The gateway issues codes to its own machine only — a request over
    // loopback, or one arriving from the address it is bound to — and the
    // recorded address is reachable from here either way: a wildcard binding
    // over loopback, a specific address from itself.
    let client = GatewayClient::new(&meta.address);
    let issued = client.issue_pairing_code().await?;
    println!("Pairing code: {}", issued.pairing_code);
    println!(
        "Enter it in the pairing prompt on the device you are connecting from, before {}.",
        issued.expires_at.format("%H:%M:%S UTC")
    );
    Ok(())
}

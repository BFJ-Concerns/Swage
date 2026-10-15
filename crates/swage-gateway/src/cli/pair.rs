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

    // The code is issued to loopback requests only, so the gateway is reached
    // at its loopback address whatever address it is bound to.
    let client = GatewayClient::new(&loopback_address(&meta.address)?);
    let issued = client.issue_pairing_code().await?;
    println!("Pairing code: {}", issued.pairing_code);
    println!(
        "Enter it in the pairing prompt on the device you are connecting from, before {}.",
        issued.expires_at.format("%H:%M:%S UTC")
    );
    Ok(())
}

/// The loopback form of a gateway's listening address: the same scheme and
/// port, the host replaced by `127.0.0.1`.
fn loopback_address(address: &str) -> Result<String, ClientError> {
    let (scheme, rest) =
        address
            .split_once("://")
            .ok_or_else(|| ClientError::GatewayNotRunning {
                reason: format!("the recorded gateway address {address:?} has no scheme"),
            })?;
    let authority = rest.split('/').next().unwrap_or_default();
    let port = authority
        .rsplit_once(':')
        .map(|(_, port)| port)
        .filter(|port| !port.is_empty() && port.chars().all(|c| c.is_ascii_digit()))
        .ok_or_else(|| ClientError::GatewayNotRunning {
            reason: format!("the recorded gateway address {address:?} has no port"),
        })?;
    Ok(format!("{scheme}://127.0.0.1:{port}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_binding_beyond_loopback_is_paired_through_its_loopback_address() {
        assert_eq!(
            loopback_address("http://0.0.0.0:3000").unwrap(),
            "http://127.0.0.1:3000"
        );
        assert_eq!(
            loopback_address("http://192.168.1.20:38417/").unwrap(),
            "http://127.0.0.1:38417"
        );
        assert_eq!(
            loopback_address("http://127.0.0.1:3000").unwrap(),
            "http://127.0.0.1:3000"
        );
    }

    #[test]
    fn an_address_without_a_port_or_scheme_is_refused() {
        assert!(loopback_address("http://gateway.local").is_err());
        assert!(loopback_address("127.0.0.1:3000").is_err());
    }
}

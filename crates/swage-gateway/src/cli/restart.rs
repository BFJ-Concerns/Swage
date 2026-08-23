//! `swage gateway restart` — stop then start the gateway.

use swage_client::{error::ClientError, runtime::RuntimeMetadata};

use crate::cli::{RestartArgs, StartArgs, StopArgs};

pub async fn run(args: &RestartArgs) -> Result<(), ClientError> {
    let metadata = RuntimeMetadata::load().map_err(|error| ClientError::GatewayNotRunning {
        reason: format!(
            "cannot determine the running gateway before restart: failed to read {}: {error}",
            RuntimeMetadata::metadata_path().display()
        ),
    })?;
    if metadata.is_stale() {
        return Err(ClientError::GatewayNotRunning {
            reason: format!(
                "cannot determine the running gateway before restart: PID {} is no longer running",
                metadata.pid
            ),
        });
    }
    let environment = super::recovery::recover_gateway_lifecycle_environment(metadata.pid)
        .map_err(|reason| ClientError::GatewayNotRunning {
            reason: format!(
                "cannot preserve the running gateway's data root and runtime directory; restart was not attempted: {reason}"
            ),
        })?;

    let stop_args = StopArgs { force: args.force };
    if let Err(error) = super::stop::run(&stop_args).await
        && !matches!(error, ClientError::GatewayNotRunning { .. })
    {
        return Err(error);
    }

    let start_args = StartArgs {
        port: args.port,
        host: args.host.clone(),
        config: args.config.clone(),
        foreground: args.foreground,
        log_file: args.log_file.clone(),
    };
    super::start::run_with_environment(&start_args, Some(&environment)).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn restart_fails_before_stop_when_gateway_provenance_is_unavailable() {
        let _runtime_dir = crate::test_support::XdgRuntimeDirGuard::new();
        let args = RestartArgs {
            force: false,
            port: None,
            host: None,
            config: None,
            foreground: false,
            log_file: None,
        };

        let error = run(&args).await.unwrap_err().to_string();

        assert!(error.contains("cannot determine the running gateway before restart"));
        assert!(error.contains("restart was not attempted") || error.contains("failed to read"));
    }
}

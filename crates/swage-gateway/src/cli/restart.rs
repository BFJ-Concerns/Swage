//! `swage gateway restart` — stop then start the gateway.

use swage_client::{error::ClientError, runtime::RuntimeMetadata};

use crate::cli::{RestartArgs, StartArgs, StopArgs};

pub async fn run(args: &RestartArgs) -> Result<(), ClientError> {
    let stop_args = StopArgs { force: args.force };
    let environment = match running_gateway_metadata(args).await? {
        Some((metadata, metadata_path)) => {
            let environment =
                super::recovery::recover_gateway_lifecycle_environment(metadata.pid).map_err(
                    |reason| ClientError::GatewayRestartRefused {
                        reason: format!(
                            "cannot preserve the running gateway's data root and runtime directory: {reason}"
                        ),
                    },
                )?;
            super::stop::run_with_metadata(&stop_args, &metadata, &metadata_path).await?;
            Some(environment)
        }
        None => {
            if let Err(error) = super::stop::run(&stop_args).await
                && !matches!(error, ClientError::GatewayNotRunning { .. })
            {
                return Err(error);
            }
            None
        }
    };

    let start_args = StartArgs {
        port: args.port,
        host: args.host.clone(),
        config: args.config.clone(),
        foreground: args.foreground,
        log_file: args.log_file.clone(),
    };
    super::start::run_with_environment(&start_args, environment.as_ref()).await
}

async fn running_gateway_metadata(
    args: &RestartArgs,
) -> Result<Option<(RuntimeMetadata, std::path::PathBuf)>, ClientError> {
    let metadata_path = RuntimeMetadata::metadata_path();
    match RuntimeMetadata::load_from(&metadata_path) {
        Ok(metadata) if !metadata.is_stale() => Ok(Some((metadata, metadata_path))),
        Ok(_) => Ok(None),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let installed = super::install::load_install_config()?;
            Ok(super::recovery::recover_configured_gateway_metadata(
                args.host
                    .as_deref()
                    .or_else(|| installed.as_ref().and_then(|config| config.host.as_deref())),
                args.port
                    .or_else(|| installed.as_ref().and_then(|config| config.port)),
            )
            .await
            .map(|metadata| (metadata, metadata_path)))
        }
        Err(source) => Err(ClientError::MetadataIo { source }),
    }
}

#[cfg(test)]
mod tests {
    use std::{process::Command, time::Duration};

    use super::*;

    #[cfg(target_os = "linux")]
    async fn wait_for_empty_process_environment(pid: u32) {
        let environment_path = format!("/proc/{pid}/environ");
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);

        loop {
            let environment =
                std::fs::read(&environment_path).expect("read spawned process environment");
            if environment.is_empty() {
                return;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "spawned process did not expose its cleared environment"
            );
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn restart_fails_before_stop_when_gateway_provenance_is_unavailable() {
        let _runtime_dir = crate::test_support::XdgRuntimeDirGuard::new();
        let _swage_home = crate::test_support::SwageHomeGuard::new();
        let mut gateway = Command::new("/bin/sleep")
            .arg("60")
            .env_clear()
            .spawn()
            .unwrap();
        wait_for_empty_process_environment(gateway.id()).await;
        RuntimeMetadata::new(gateway.id(), "http://127.0.0.1:9".to_string())
            .save()
            .unwrap();
        let args = RestartArgs {
            force: true,
            port: None,
            host: None,
            config: None,
            foreground: false,
            log_file: None,
        };

        let error = run(&args).await.unwrap_err();

        match error {
            ClientError::GatewayRestartRefused { reason } => {
                assert!(reason.contains("exposes neither SWAGE_HOME nor HOME"));
            }
            other => panic!("expected GatewayRestartRefused, got: {other:?}"),
        }
        assert!(gateway.try_wait().unwrap().is_none(), "gateway was stopped");

        gateway.kill().unwrap();
        gateway.wait().unwrap();
        RuntimeMetadata::remove().unwrap();
    }
}

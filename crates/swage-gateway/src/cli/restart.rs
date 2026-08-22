//! `swage gateway restart` — stop then start the gateway.

use swage_client::error::ClientError;

use crate::cli::{RestartArgs, StartArgs, StopArgs};

pub async fn run(args: &RestartArgs) -> Result<(), ClientError> {
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
    super::start::run(&start_args).await
}

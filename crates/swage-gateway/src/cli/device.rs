//! `swage device connect` — run this machine as a device.
//!
//! The process dials the gateway, presents the registration's shared token,
//! advertises the workspace tools, and serves operations until interrupted.
//! Registration or pairing happens before this, once, through the gateway's
//! device routes; the token file holds the value of the secret that
//! registration named.

use std::path::PathBuf;
use std::sync::Arc;

use swage_client::error::ClientError;
use swage_device::{DEFAULT_RECONNECT_DELAY, DeviceConnection};
use tokio_util::sync::CancellationToken;

/// Arguments for `swage device <subcommand>`.
#[derive(Debug, clap::Args)]
pub struct DeviceArgs {
    #[command(subcommand)]
    pub command: DeviceCommand,
}

#[derive(Debug, clap::Subcommand)]
pub enum DeviceCommand {
    /// Dial the gateway and serve its operations until interrupted.
    Connect(ConnectArgs),
}

/// Arguments for `swage device connect`.
#[derive(Debug, clap::Args)]
pub struct ConnectArgs {
    /// The gateway's HTTP base URL, e.g. `http://gateway.local:3000`.
    #[arg(long)]
    pub gateway: String,

    /// The device id this machine registered under.
    #[arg(long)]
    pub device_id: String,

    /// A label for the device, shown beside its id.
    #[arg(long)]
    pub label: Option<String>,

    /// A file holding the shared token — the value of the gateway secret the
    /// registration named — and nothing else.
    #[arg(long)]
    pub shared_token_file: PathBuf,

    /// The directory the device operates in; operations whose context lies
    /// outside it are refused. Defaults to the current directory.
    #[arg(long)]
    pub workspace: Option<PathBuf>,
}

pub async fn connect(args: &ConnectArgs) -> Result<(), ClientError> {
    let failed = |message: String| ClientError::WebSocketFailed {
        address: args.gateway.clone(),
        message,
    };
    let shared_token = std::fs::read_to_string(&args.shared_token_file)
        .map_err(|error| {
            failed(format!(
                "could not read the shared token from {}: {error}",
                args.shared_token_file.display()
            ))
        })?
        .trim()
        .to_owned();
    if shared_token.is_empty() {
        return Err(failed(format!(
            "{} holds no shared token",
            args.shared_token_file.display()
        )));
    }
    let workspace = match &args.workspace {
        Some(path) => path.clone(),
        None => std::env::current_dir().map_err(|error| failed(error.to_string()))?,
    };
    let workspace = workspace
        .canonicalize()
        .map_err(|error| failed(format!("workspace {}: {error}", workspace.display())))?;

    let config = swage_config::ResolvedConfig::default();
    let dispatcher =
        crate::tool::ToolDispatcher::allow_all(crate::startup::workspace_tools(&config));
    let connection = Arc::new(DeviceConnection {
        gateway_url: args.gateway.clone(),
        device_id: args.device_id.clone(),
        shared_token,
        label: args.label.clone(),
        dispatcher: Arc::new(dispatcher),
        workspace_roots: vec![workspace.clone()],
        reconnect_delay: DEFAULT_RECONNECT_DELAY,
    });

    let shutdown = CancellationToken::new();
    tokio::spawn({
        let shutdown = shutdown.clone();
        async move {
            if tokio::signal::ctrl_c().await.is_ok() {
                shutdown.cancel();
            }
        }
    });

    println!(
        "device '{}' serving {} from {}",
        args.device_id,
        args.gateway,
        workspace.display()
    );
    connection
        .run(shutdown)
        .await
        .map_err(|error| failed(error.to_string()))
}

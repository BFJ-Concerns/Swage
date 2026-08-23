//! `swage gateway set-defaults` and `clear-defaults` support.
//!
//! Stores operator-selected service defaults locally so lifecycle commands can
//! reuse one canonical configuration.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use swage_client::error::ClientError;

use crate::cli::{InstallArgs, UninstallArgs};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct ServiceInstallConfig {
    pub port: Option<u16>,
    pub host: Option<String>,
    pub config: Option<String>,
}

pub fn install_config_path() -> PathBuf {
    swage_config::discovery::resolve_root()
        .unwrap_or_else(|_| swage_config::discovery::effective_root_path())
        .join("service.json")
}

fn load_install_config_at_path(path: PathBuf) -> Result<Option<ServiceInstallConfig>, ClientError> {
    match std::fs::read_to_string(&path) {
        Ok(content) => {
            serde_json::from_str(&content)
                .map(Some)
                .map_err(|source| ClientError::MetadataIo {
                    source: std::io::Error::new(std::io::ErrorKind::InvalidData, source),
                })
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(ClientError::MetadataIo { source }),
    }
}

pub(super) fn load_install_config_for_root(
    root: &std::path::Path,
) -> Result<Option<ServiceInstallConfig>, ClientError> {
    load_install_config_at_path(root.join("service.json"))
}

pub fn load_install_config() -> Result<Option<ServiceInstallConfig>, ClientError> {
    load_install_config_at_path(install_config_path())
}

pub async fn install(args: &InstallArgs) -> Result<(), ClientError> {
    install_at_path(args, install_config_path()).await
}

async fn install_at_path(args: &InstallArgs, path: PathBuf) -> Result<(), ClientError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| ClientError::MetadataIo { source })?;
    }
    let config = ServiceInstallConfig {
        port: args.port,
        host: args.host.clone(),
        config: args.config.clone(),
    };
    let json = serde_json::to_string_pretty(&config).map_err(|source| ClientError::MetadataIo {
        source: std::io::Error::new(std::io::ErrorKind::InvalidData, source),
    })?;
    std::fs::write(&path, json).map_err(|source| ClientError::MetadataIo { source })?;
    println!("Gateway lifecycle defaults saved at {}", path.display());
    Ok(())
}

pub async fn uninstall(_args: &UninstallArgs) -> Result<(), ClientError> {
    uninstall_at_path(install_config_path()).await
}

async fn uninstall_at_path(path: PathBuf) -> Result<(), ClientError> {
    match std::fs::remove_file(&path) {
        Ok(()) => {
            println!("Gateway lifecycle defaults removed");
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            println!("Gateway lifecycle defaults were not set");
            Ok(())
        }
        Err(source) => Err(ClientError::MetadataIo { source }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_path_ends_with_service_json() {
        let path = install_config_path();
        assert_eq!(
            path.file_name().and_then(|name| name.to_str()),
            Some("service.json")
        );
    }

    #[tokio::test]
    async fn install_and_load_round_trip() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("service.json");

        install_at_path(
            &InstallArgs {
                port: Some(4000),
                host: Some("127.0.0.1".into()),
                config: Some("/tmp/swage.toml".into()),
            },
            path.clone(),
        )
        .await
        .unwrap();

        assert_eq!(
            load_install_config_at_path(path).unwrap(),
            Some(ServiceInstallConfig {
                port: Some(4000),
                host: Some("127.0.0.1".into()),
                config: Some("/tmp/swage.toml".into()),
            })
        );
    }

    #[tokio::test]
    async fn uninstall_is_idempotent() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("service.json");

        uninstall_at_path(path.clone()).await.unwrap();
        install_at_path(
            &InstallArgs {
                port: None,
                host: None,
                config: None,
            },
            path.clone(),
        )
        .await
        .unwrap();
        uninstall_at_path(path.clone()).await.unwrap();
        assert_eq!(load_install_config_at_path(path).unwrap(), None);
    }
}

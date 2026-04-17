//! `swage gateway install` and `uninstall` support.
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
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(home).join(".swage").join("service.json")
}

pub fn load_install_config() -> Result<Option<ServiceInstallConfig>, ClientError> {
    let path = install_config_path();
    match std::fs::read_to_string(&path) {
        Ok(content) => serde_json::from_str(&content).map(Some).map_err(|source| {
            ClientError::MetadataIo {
                source: std::io::Error::new(std::io::ErrorKind::InvalidData, source),
            }
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(ClientError::MetadataIo { source }),
    }
}

pub async fn install(args: &InstallArgs) -> Result<(), ClientError> {
    let path = install_config_path();
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
    println!("Gateway service defaults installed at {}", path.display());
    Ok(())
}

pub async fn uninstall(_args: &UninstallArgs) -> Result<(), ClientError> {
    let path = install_config_path();
    match std::fs::remove_file(&path) {
        Ok(()) => {
            println!("Gateway service defaults removed");
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            println!("Gateway service defaults were not installed");
            Ok(())
        }
        Err(source) => Err(ClientError::MetadataIo { source }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn install_path_ends_with_service_json() {
        let path = install_config_path();
        assert!(path.to_string_lossy().ends_with(".swage/service.json"));
    }

    #[tokio::test]
    async fn install_and_load_round_trip() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::TempDir::new().unwrap();
        unsafe { std::env::set_var("HOME", dir.path()) };

        install(&InstallArgs {
            port: Some(4000),
            host: Some("127.0.0.1".into()),
            config: Some("/tmp/swage.toml".into()),
        })
        .await
        .unwrap();

        assert_eq!(
            load_install_config().unwrap(),
            Some(ServiceInstallConfig {
                port: Some(4000),
                host: Some("127.0.0.1".into()),
                config: Some("/tmp/swage.toml".into()),
            })
        );

        unsafe { std::env::remove_var("HOME") };
    }

    #[tokio::test]
    async fn uninstall_is_idempotent() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::TempDir::new().unwrap();
        unsafe { std::env::set_var("HOME", dir.path()) };

        uninstall(&UninstallArgs {}).await.unwrap();
        install(&InstallArgs {
            port: None,
            host: None,
            config: None,
        })
        .await
        .unwrap();
        uninstall(&UninstallArgs {}).await.unwrap();
        assert_eq!(load_install_config().unwrap(), None);

        unsafe { std::env::remove_var("HOME") };
    }
}

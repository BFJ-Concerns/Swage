//! Preserve systemd ownership when restarting a supervised gateway.

use std::time::Duration;

use swage_client::{error::ClientError, http::GatewayClient};
use tokio::process::Command;

use super::RestartArgs;

pub(super) struct GatewayService {
    unit: String,
    user: bool,
}

fn refused(reason: impl Into<String>) -> ClientError {
    ClientError::GatewayRestartRefused {
        reason: reason.into(),
    }
}

impl GatewayService {
    fn command(&self) -> Command {
        let mut command = Command::new("systemctl");
        if self.user {
            command.arg("--user");
        }
        command.arg("--no-ask-password");
        command
    }

    pub(super) async fn restart(
        &self,
        args: &RestartArgs,
        address: &str,
    ) -> Result<(), ClientError> {
        if args.port.is_some()
            || args.host.is_some()
            || args.config.is_some()
            || args.foreground
            || args.log_file.is_some()
            || args.force
        {
            return Err(refused(format!(
                "{} owns this gateway; change its service configuration before restarting without CLI overrides",
                self.unit
            )));
        }
        let output = self
            .command()
            .args(["restart", "--", &self.unit])
            .output()
            .await
            .map_err(|error| refused(error.to_string()))?;
        if !output.status.success() {
            return Err(refused(format!(
                "systemd restart of {} failed: {}",
                self.unit,
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        let client = GatewayClient::new(address);
        let ready = async {
            loop {
                if client.health().await.is_ok() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
        };
        tokio::time::timeout(Duration::from_secs(30), ready).await.map_err(|_| {
            refused(format!("{} restarted but the gateway did not become healthy within 30 seconds; inspect the service journal", self.unit))
        })?;
        println!("Gateway restarted through {} and is healthy", self.unit);
        Ok(())
    }
}

/// A service cgroup alone is insufficient: detached processes can inherit a
/// terminal's cgroup. Only the unit's main PID establishes gateway ownership.
pub(super) async fn gateway_service(pid: u32) -> Result<Option<GatewayService>, ClientError> {
    #[cfg(target_os = "linux")]
    {
        let cgroup = std::fs::read_to_string(format!("/proc/{pid}/cgroup"))
            .map_err(|error| refused(format!("cannot determine gateway supervision: {error}")))?;
        let Some(service) = service_from_cgroup(&cgroup) else {
            return Ok(None);
        };
        let output = service
            .command()
            .args(["show", "--property=MainPID", "--value", "--", &service.unit])
            .output()
            .await
            .map_err(|error| refused(format!("cannot inspect gateway supervisor: {error}")))?;
        if !output.status.success() {
            return Err(refused(format!(
                "cannot inspect gateway service {}: {}",
                service.unit,
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        let main_pid = String::from_utf8_lossy(&output.stdout)
            .trim()
            .parse::<u32>()
            .map_err(|error| refused(format!("invalid gateway service MainPID: {error}")))?;
        Ok((main_pid == pid).then_some(service))
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = pid;
        Ok(None)
    }
}

#[cfg(any(target_os = "linux", test))]
fn service_from_cgroup(contents: &str) -> Option<GatewayService> {
    for line in contents.lines() {
        let mut fields = line.splitn(3, ':');
        let hierarchy = fields.next()?;
        let controllers = fields.next()?;
        let path = fields.next()?;
        if hierarchy != "0" && !controllers.split(',').any(|value| value == "name=systemd") {
            continue;
        }
        let components: Vec<_> = path.split('/').filter(|part| !part.is_empty()).collect();
        let unit = components.last()?;
        if !unit.ends_with(".service") || unit.starts_with("user@") {
            continue;
        }
        return Some(GatewayService {
            unit: (*unit).to_owned(),
            user: components
                .iter()
                .any(|part| part.starts_with("user@") && part.ends_with(".service")),
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognises_user_and_system_services_without_treating_scopes_as_services() {
        let user = service_from_cgroup(
            "0::/user.slice/user-1000.slice/user@1000.service/app.slice/swage-gateway.service\n",
        )
        .unwrap();
        assert!(user.user);
        assert_eq!(user.unit, "swage-gateway.service");
        let system = service_from_cgroup("1:name=systemd:/system.slice/swage.service\n").unwrap();
        assert!(!system.user);
        assert_eq!(system.unit, "swage.service");
        assert!(
            service_from_cgroup("0::/user.slice/user@1000.service/app.slice/terminal.scope\n")
                .is_none()
        );
        assert!(service_from_cgroup("0::/user.slice/user@1000.service\n").is_none());
    }

    #[tokio::test]
    async fn rejects_overrides_before_contacting_service_manager() {
        let service = GatewayService {
            unit: "nonexistent-readiness.service".into(),
            user: true,
        };
        let args = RestartArgs {
            force: false,
            port: Some(4000),
            host: None,
            config: None,
            foreground: false,
            log_file: None,
        };
        let error = service
            .restart(&args, "http://127.0.0.1:9")
            .await
            .unwrap_err();
        assert!(error.to_string().contains("without CLI overrides"));
    }
}

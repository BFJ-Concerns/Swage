//! Preserve systemd ownership when restarting a supervised gateway.

use std::{path::Path, time::Duration};

use swage_client::{error::ClientError, http::GatewayClient, runtime::RuntimeMetadata};
use tokio::process::Command;

use super::RestartArgs;

const RESTART_HEALTH_TIMEOUT: Duration = Duration::from_secs(30);

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
        previous: &RuntimeMetadata,
        metadata_path: &Path,
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
            return Err(ClientError::GatewayRestartFailed {
                reason: format!(
                    "systemd restart of {} failed: {}",
                    self.unit,
                    String::from_utf8_lossy(&output.stderr).trim()
                ),
            });
        }
        let address =
            wait_for_restarted_gateway(metadata_path, previous.pid, RESTART_HEALTH_TIMEOUT)
                .await
                .map_err(|reason| ClientError::GatewayRestartFailed {
                    reason: format!(
                        "{} restarted but {reason}; inspect the service journal",
                        self.unit
                    ),
                })?;
        println!(
            "Gateway restarted through {} and is healthy on {address}",
            self.unit
        );
        Ok(())
    }
}

/// The address the restarted gateway is serving, once it is healthy there.
///
/// The service configuration may bind the new process somewhere other than
/// the old one, so the address is read from the runtime metadata the new
/// process writes rather than carried over from before the restart. A record
/// still naming the previous process is the old gateway's, not evidence of
/// the new one, and is skipped: the old process may still be draining, or a
/// stranger may answer on its former address.
async fn wait_for_restarted_gateway(
    metadata_path: &Path,
    previous_pid: u32,
    timeout: Duration,
) -> Result<String, String> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if let Ok(metadata) = RuntimeMetadata::load_from(metadata_path)
            && metadata.pid != previous_pid
            && GatewayClient::new(&metadata.address).health().await.is_ok()
        {
            return Ok(metadata.address);
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(format!(
                "no new gateway process recorded a healthy address within {} seconds",
                timeout.as_secs()
            ));
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
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
        let previous = RuntimeMetadata::new(1, "http://127.0.0.1:9".into());
        let error = service
            .restart(&args, &previous, Path::new("/nonexistent/metadata.json"))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("without CLI overrides"));
    }

    async fn serve_health() -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let router = axum::Router::new().route(
            "/api/health",
            axum::routing::get(|| async { axum::Json(serde_json::json!({"status": "ok"})) }),
        );
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        address
    }

    #[tokio::test]
    async fn restart_waits_for_the_new_process_at_the_address_it_records() {
        let dir = tempfile::tempdir().unwrap();
        let metadata_path = dir.path().join("metadata.json");
        let old_address = serve_health().await;
        let new_address = serve_health().await;
        assert_ne!(old_address, new_address);
        // The old process's record, still healthy on its address, is not
        // the restarted gateway.
        RuntimeMetadata::new(1000, old_address.clone())
            .save_to(&metadata_path)
            .unwrap();
        let timed_out =
            wait_for_restarted_gateway(&metadata_path, 1000, Duration::from_millis(600))
                .await
                .unwrap_err();
        assert!(timed_out.contains("no new gateway process"), "{timed_out}");

        // The new process records a different pid and address; the wait
        // reports where it is actually serving.
        let writer = {
            let metadata_path = metadata_path.clone();
            let new_address = new_address.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(300)).await;
                RuntimeMetadata::new(1001, new_address)
                    .save_to(&metadata_path)
                    .unwrap();
            })
        };
        let found = wait_for_restarted_gateway(&metadata_path, 1000, Duration::from_secs(5))
            .await
            .unwrap();
        writer.await.unwrap();
        assert_eq!(found, new_address);
    }
}

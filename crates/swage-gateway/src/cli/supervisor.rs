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

trait ServiceManager {
    async fn output(
        &self,
        service: &GatewayService,
        args: &[&str],
    ) -> std::io::Result<std::process::Output>;
}

struct Systemd;
impl ServiceManager for Systemd {
    async fn output(
        &self,
        service: &GatewayService,
        args: &[&str],
    ) -> std::io::Result<std::process::Output> {
        service.command().args(args).output().await
    }
}

trait HealthProbe {
    async fn healthy(&self, address: &str) -> bool;
}

struct HttpHealthProbe;
impl HealthProbe for HttpHealthProbe {
    async fn healthy(&self, address: &str) -> bool {
        GatewayClient::new(address).health().await.is_ok()
    }
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
        self.restart_with(args, previous, metadata_path, &Systemd, &HttpHealthProbe)
            .await
    }

    async fn restart_with(
        &self,
        args: &RestartArgs,
        previous: &RuntimeMetadata,
        metadata_path: &Path,
        manager: &impl ServiceManager,
        probe: &impl HealthProbe,
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
        let output = manager
            .output(self, &["restart", "--", &self.unit])
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
        let address = wait_for_restarted_gateway_with(
            metadata_path,
            previous.pid,
            RESTART_HEALTH_TIMEOUT,
            probe,
        )
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
#[cfg(test)]
async fn wait_for_restarted_gateway(
    metadata_path: &Path,
    previous_pid: u32,
    timeout: Duration,
) -> Result<String, String> {
    wait_for_restarted_gateway_with(metadata_path, previous_pid, timeout, &HttpHealthProbe).await
}

async fn wait_for_restarted_gateway_with(
    metadata_path: &Path,
    previous_pid: u32,
    timeout: Duration,
    probe: &impl HealthProbe,
) -> Result<String, String> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if tokio::time::Instant::now() >= deadline {
            return Err(format!(
                "no new gateway process recorded a healthy address within {} seconds",
                timeout.as_secs()
            ));
        }
        if let Ok(metadata) = RuntimeMetadata::load_from(metadata_path)
            && metadata.pid != previous_pid
            && matches!(
                tokio::time::timeout_at(deadline, probe.healthy(&metadata.address)).await,
                Ok(true)
            )
        {
            return Ok(metadata.address);
        }
        tokio::time::sleep_until(
            (tokio::time::Instant::now() + Duration::from_millis(250)).min(deadline),
        )
        .await;
    }
}

/// A service cgroup alone is insufficient: detached processes can inherit a
/// terminal's cgroup. Only the unit's main PID establishes gateway ownership.
pub(super) async fn gateway_service(pid: u32) -> Result<Option<GatewayService>, ClientError> {
    #[cfg(target_os = "linux")]
    {
        let cgroup = std::fs::read_to_string(format!("/proc/{pid}/cgroup"))
            .map_err(|error| refused(format!("cannot determine gateway supervision: {error}")))?;
        service_for_cgroup(pid, &cgroup, &Systemd).await
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = pid;
        Ok(None)
    }
}

#[cfg(any(target_os = "linux", test))]
async fn service_for_cgroup(
    pid: u32,
    cgroup: &str,
    manager: &impl ServiceManager,
) -> Result<Option<GatewayService>, ClientError> {
    let Some(service) = service_from_cgroup(cgroup) else {
        return Ok(None);
    };
    let output = manager
        .output(
            &service,
            &["show", "--property=MainPID", "--value", "--", &service.unit],
        )
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

    struct FakeManager {
        success: bool,
        main_pid: u32,
        calls: std::cell::RefCell<Vec<Vec<String>>>,
    }

    impl ServiceManager for FakeManager {
        fn output(
            &self,
            _service: &GatewayService,
            args: &[&str],
        ) -> impl Future<Output = std::io::Result<std::process::Output>> {
            #[cfg(unix)]
            use std::os::unix::process::ExitStatusExt;
            #[cfg(windows)]
            use std::os::windows::process::ExitStatusExt;
            self.calls
                .borrow_mut()
                .push(args.iter().map(|arg| (*arg).to_owned()).collect());
            std::future::ready(Ok(std::process::Output {
                status: std::process::ExitStatus::from_raw(if self.success { 0 } else { 256 }),
                stdout: self.main_pid.to_string().into_bytes(),
                stderr: b"service start refused".to_vec(),
            }))
        }
    }

    struct Probe {
        healthy: bool,
        delay: Duration,
    }
    impl HealthProbe for Probe {
        async fn healthy(&self, _address: &str) -> bool {
            tokio::time::sleep(self.delay).await;
            self.healthy
        }
    }

    fn manager(success: bool, main_pid: u32) -> FakeManager {
        FakeManager {
            success,
            main_pid,
            calls: Default::default(),
        }
    }

    fn restart_args() -> RestartArgs {
        RestartArgs {
            force: false,
            port: None,
            host: None,
            config: None,
            foreground: false,
            log_file: None,
        }
    }

    #[tokio::test]
    async fn main_pid_alone_establishes_service_ownership() {
        let cgroup = "0::/system.slice/swage.service\n";
        let manager = manager(true, 42);
        assert!(
            service_for_cgroup(42, cgroup, &manager)
                .await
                .unwrap()
                .is_some()
        );
        assert!(
            service_for_cgroup(43, cgroup, &manager)
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(
            manager.calls.borrow()[0],
            [
                "show",
                "--property=MainPID",
                "--value",
                "--",
                "swage.service"
            ]
        );
    }

    #[tokio::test]
    async fn failed_restart_reports_the_service_managers_error() {
        let service = GatewayService {
            unit: "swage.service".into(),
            user: false,
        };
        let manager = manager(false, 42);
        let error = service
            .restart_with(
                &restart_args(),
                &RuntimeMetadata::new(42, "http://unused".into()),
                Path::new("/nonexistent/metadata.json"),
                &manager,
                &Probe {
                    healthy: true,
                    delay: Duration::ZERO,
                },
            )
            .await
            .unwrap_err();
        assert!(
            matches!(&error, ClientError::GatewayRestartFailed { reason } if reason.contains("service start refused")),
            "{error}"
        );
        assert_eq!(
            manager.calls.borrow()[0],
            ["restart", "--", "swage.service"]
        );
    }

    #[tokio::test]
    async fn all_six_overrides_are_refused_before_any_service_command() {
        let service = GatewayService {
            unit: "swage.service".into(),
            user: false,
        };
        for index in 0..6 {
            let mut args = restart_args();
            match index {
                0 => args.port = Some(4321),
                1 => args.host = Some("127.0.0.1".into()),
                2 => args.config = Some("other".into()),
                3 => args.foreground = true,
                4 => args.log_file = Some("other.log".into()),
                _ => args.force = true,
            }
            let manager = manager(false, 42);
            let error = service
                .restart_with(
                    &args,
                    &RuntimeMetadata::new(42, "http://unused".into()),
                    Path::new("/nonexistent/metadata.json"),
                    &manager,
                    &Probe {
                        healthy: false,
                        delay: Duration::ZERO,
                    },
                )
                .await
                .unwrap_err();
            assert!(
                matches!(error, ClientError::GatewayRestartRefused { .. }),
                "override {index}: {error}"
            );
            assert!(
                manager.calls.borrow().is_empty(),
                "override {index} contacted systemd"
            );
        }
    }

    #[tokio::test]
    async fn slow_probe_cannot_outlive_the_remaining_readiness_deadline() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("metadata.json");
        RuntimeMetadata::new(43, "http://unused".into())
            .save_to(&path)
            .unwrap();
        let result = tokio::time::timeout(
            Duration::from_millis(250),
            wait_for_restarted_gateway_with(
                &path,
                42,
                Duration::from_millis(30),
                &Probe {
                    healthy: true,
                    delay: Duration::from_secs(2),
                },
            ),
        )
        .await;
        assert!(
            result.is_ok(),
            "health probe exceeded the readiness deadline"
        );
        assert!(
            result.unwrap().is_err(),
            "a late healthy response must not admit the gateway"
        );
    }

    #[tokio::test]
    async fn unhealthy_replacement_is_not_admitted_by_its_new_pid() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("metadata.json");
        RuntimeMetadata::new(43, "http://unused".into())
            .save_to(&path)
            .unwrap();
        let result = wait_for_restarted_gateway_with(
            &path,
            42,
            Duration::from_millis(30),
            &Probe {
                healthy: false,
                delay: Duration::ZERO,
            },
        )
        .await;
        assert!(result.is_err(), "an unhealthy replacement was admitted");
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

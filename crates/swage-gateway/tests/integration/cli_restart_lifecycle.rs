#![cfg(target_os = "linux")]

use std::{
    cell::Cell,
    ffi::CString,
    fs,
    io::Read,
    net::{Ipv4Addr, SocketAddr},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::ffi::OsStrExt,
    },
    panic::AssertUnwindSafe,
    path::{Path, PathBuf},
    process::Output,
};

use futures::FutureExt;
use serde_json::Value;

use swage_client::{http::GatewayClient, runtime::RuntimeMetadata};
use tokio::{io::unix::AsyncFd, process::Command};

use super::support::journey;

struct GatewayProbe {
    binary: PathBuf,
    root: tempfile::TempDir,
    runtime: tempfile::TempDir,
    home: tempfile::TempDir,
    port: Cell<u16>,
    explicit_root: bool,
    xdg_runtime: bool,
    metadata_events: AsyncFd<fs::File>,
}

impl GatewayProbe {
    fn new() -> Self {
        Self::with_environment(true, true)
    }

    fn without_explicit_root() -> Self {
        Self::with_environment(false, true)
    }

    fn with_default_root() -> Self {
        Self::with_environment(false, false)
    }

    fn with_environment(explicit_root: bool, xdg_runtime: bool) -> Self {
        let root = tempfile::TempDir::new().expect("failed to create the probe root");
        let runtime = super::support::scratch_tempdir();
        let home = tempfile::TempDir::new().expect("failed to create the probe home");
        let metadata_directory = if explicit_root {
            root.path().join("run")
        } else if xdg_runtime {
            runtime.path().join("swage")
        } else {
            home.path().join(".swage/run")
        };
        fs::create_dir_all(&metadata_directory).expect("create metadata watch directory");
        let metadata_events = watch_metadata(&metadata_directory);
        Self {
            binary: PathBuf::from(env!("CARGO_BIN_EXE_swage")),
            root,
            runtime,
            home,
            metadata_events,
            port: Cell::new(0),
            explicit_root,
            xdg_runtime,
        }
    }

    fn command(&self) -> Command {
        let mut command = Command::new(&self.binary);
        command.kill_on_drop(true);
        command.env("HOME", self.home.path());
        if self.explicit_root {
            command.env("SWAGE_HOME", self.root.path());
        } else {
            command.env_remove("SWAGE_HOME");
        }
        if self.xdg_runtime {
            command.env("XDG_RUNTIME_DIR", self.runtime.path());
        } else {
            command.env_remove("XDG_RUNTIME_DIR");
        }
        command
    }

    async fn run(&self, arguments: &[&str]) -> Output {
        self.command()
            .args(arguments)
            .output()
            .await
            .expect("failed to invoke the Swage CLI")
    }

    fn assert_success(&self, output: Output) {
        assert!(
            output.status.success(),
            "command failed\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    async fn assert_ready_after_start(
        &self,
        output: Output,
        previous_pid: Option<u32>,
        requested_port: u16,
    ) {
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success() || stderr.contains("gateway did not become ready within 5s"),
            "gateway start failed before readiness\nstdout: {}\nstderr: {stderr}",
            String::from_utf8_lossy(&output.stdout),
        );
        self.wait_for_ready_process(previous_pid, requested_port)
            .await;
    }

    async fn wait_for_ready_process(&self, previous_pid: Option<u32>, requested_port: u16) -> u32 {
        loop {
            if let Ok(metadata) = RuntimeMetadata::load_from(&self.metadata_path())
                && Some(metadata.pid) != previous_pid
            {
                assert!(
                    !metadata.is_stale(),
                    "gateway published a dead PID: {metadata:?}"
                );
                let address: SocketAddr = metadata
                    .address
                    .strip_prefix("http://")
                    .expect("gateway publishes an HTTP address")
                    .parse()
                    .expect("gateway publishes its bound socket address");
                assert_eq!(address.ip(), Ipv4Addr::LOCALHOST);
                assert_ne!(
                    address.port(),
                    0,
                    "gateway must publish the actual assigned port"
                );
                if requested_port != 0 {
                    assert_eq!(address.port(), requested_port);
                }
                // Startup publishes metadata after binding. The live request can
                // wait for the server to accept it; a failed health check is a
                // product failure, not a reason to retry or extend a deadline.
                GatewayClient::new(&metadata.address)
                    .health()
                    .await
                    .expect("published replacement must serve its health endpoint");
                self.port.set(address.port());
                return metadata.pid;
            }
            // The directory watch is installed before any CLI invocation. An
            // atomic metadata rename wakes this reader even if it occurred
            // between the read above and registering interest in the fd.
            let mut ready = self
                .metadata_events
                .readable()
                .await
                .expect("metadata event readiness");
            let mut events = [0u8; 4096];
            match ready.try_io(|fd| {
                let mut file = fd.get_ref();
                file.read(&mut events)
            }) {
                Ok(result) => {
                    result.expect("read metadata events");
                }
                Err(_) => continue,
            }
        }
    }

    fn metadata_path(&self) -> PathBuf {
        if self.explicit_root {
            self.root.path().join("run/metadata.json")
        } else if self.xdg_runtime {
            self.runtime.path().join("swage/metadata.json")
        } else {
            self.home.path().join(".swage/run/metadata.json")
        }
    }

    fn pid(&self) -> u32 {
        let bytes = fs::read(self.metadata_path()).expect("failed to read runtime metadata");
        let metadata: Value =
            serde_json::from_slice(&bytes).expect("runtime metadata was not valid JSON");
        metadata["pid"]
            .as_u64()
            .expect("runtime metadata did not contain a numeric PID") as u32
    }

    async fn start(&self) {
        self.assert_ready_after_start(
            self.run(&["gateway", "start", "--port", "0"]).await,
            None,
            0,
        )
        .await;
    }

    async fn stop(&self) {
        let output = self.run(&["gateway", "stop", "--force"]).await;
        assert!(
            output.status.success()
                || (!self.metadata_path().exists()
                    && String::from_utf8_lossy(&output.stderr).contains("no metadata file found")),
            "fixture cleanup failed\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }

    async fn journey(self, body: impl AsyncFnOnce(&Self)) {
        // Keep the disposable roots alive through asynchronous cleanup even
        // when the journey panics or its hang guard expires.
        let result = AssertUnwindSafe(journey(body(&self))).catch_unwind().await;
        let cleanup = AssertUnwindSafe(journey(self.stop())).catch_unwind().await;
        if let Err(panic) = result {
            std::panic::resume_unwind(panic);
        }
        if let Err(panic) = cleanup {
            std::panic::resume_unwind(panic);
        }
    }
}

impl Drop for GatewayProbe {
    fn drop(&mut self) {
        let data_root = if self.explicit_root {
            self.root.path().to_path_buf()
        } else {
            self.home.path().join(".swage")
        };
        eprintln!(
            "gateway fixture final state: metadata {:?}; daemon log: {}",
            fs::read_to_string(self.metadata_path()),
            fs::read_to_string(data_root.join("gateway.log")).unwrap_or_default()
        );
        // No subprocess or wait may block Drop after a timeout. The metadata
        // belongs to this disposable fixture; terminate a surviving daemon if
        // its asynchronous cleanup could not finish.
        if let Ok(metadata) = RuntimeMetadata::load_from(&self.metadata_path()) {
            let expected_home = format!("HOME={}", self.home.path().display());
            let environment =
                fs::read(format!("/proc/{}/environ", metadata.pid)).unwrap_or_default();
            if environment
                .split(|byte| *byte == 0)
                .any(|entry| entry == expected_home.as_bytes())
            {
                // SAFETY: signal only a process belonging to this disposable HOME.
                unsafe { libc::kill(metadata.pid as i32, libc::SIGKILL) };
            }
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn clean_stop_status_reports_not_running_without_an_error() {
    GatewayProbe::new()
        .journey(async |probe| {
            probe.start().await;
            probe.assert_success(probe.run(&["gateway", "stop"]).await);
            assert!(!probe.metadata_path().exists());

            let output = probe.run(&["gateway", "status"]).await;
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(
                output.status.success() && !stdout.contains("Error:") && !stderr.contains("Error:"),
                "stopped status must succeed without an error\nstdout: {stdout}\nstderr: {stderr}"
            );
            assert!(
                stdout.contains("not running") || stderr.contains("not running"),
                "stopped status must report not running\nstdout: {stdout}\nstderr: {stderr}"
            );
        })
        .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn start_after_sigkill_recovers_without_a_status_call() {
    GatewayProbe::new().journey(async |probe| {
        let mut child = probe.command()
            .args(["gateway", "start", "--foreground", "--port", "0"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn foreground gateway");
        let first_pid = tokio::select! {
            pid = probe.wait_for_ready_process(None, 0) => pid,
            status = child.wait() => panic!("foreground gateway exited before readiness: {status:?}"),
        };
        assert_eq!(Some(first_pid), child.id());
        child.start_kill().expect("SIGKILL foreground gateway");
        // Do not poll child.wait(): start must recover while the terminated
        // process is still an unreaped zombie and its metadata remains intact.
        loop {
            let status = fs::read_to_string(format!("/proc/{first_pid}/status"))
                .expect("unreaped foreground process still has a proc entry");
            if status.lines().any(|line| line.starts_with("State:") && line.contains('Z')) {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert_eq!(probe.pid(), first_pid);
        let metadata = RuntimeMetadata::load_from(&probe.metadata_path()).expect("killed gateway metadata");
        assert!(
            GatewayClient::new(&metadata.address).health().await.is_err(),
            "killed gateway must no longer serve health"
        );
        let output = probe.run(&["gateway", "start", "--port", "0"]).await;
        probe.assert_ready_after_start(output, Some(first_pid), 0).await;
        assert_ne!(probe.pid(), first_pid, "start must publish a replacement PID");
        child.wait().await.expect("reap killed foreground gateway");
    }).await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gateway_start_regenerates_fresh_and_damaged_shipped_skill_projection() {
    GatewayProbe::new()
        .journey(async |probe| {
            use std::os::unix::fs::PermissionsExt;
            let projected = probe
                .root
                .path()
                .join("generated/shipped-skills/commission/scripts/create-annexe.sh");
            let projection_root = probe.root.path().join("generated/shipped-skills");
            let expected =
                include_str!("../../../../shipped/skills/commission/scripts/create-annexe.sh");

            probe.start().await;
            assert_eq!(
                fs::read(&projected).expect("gateway startup projected shipped skill"),
                expected.as_bytes(),
                "the gateway startup consumer must project bundled skill bytes into a fresh root"
            );
            probe.stop().await;

            fs::write(&projected, "damaged projection").expect("damage projected skill");
            fs::set_permissions(&projected, fs::Permissions::from_mode(0o600))
                .expect("remove projected script execute mode");
            let stray = projection_root.join("stray.txt");
            fs::write(&stray, "stray").expect("write stray projection file");

            probe.start().await;
            assert_eq!(
                fs::read(&projected).expect("gateway startup restored shipped skill"),
                expected.as_bytes(),
                "a later gateway startup must replace damaged projected bytes"
            );
            assert!(
                !stray.exists(),
                "a later gateway startup must replace the complete shipped-skill projection"
            );
            assert_eq!(
                fs::metadata(&projected)
                    .expect("gateway startup restored projected script metadata")
                    .permissions()
                    .mode()
                    & 0o777,
                0o755,
                "a later gateway startup must restore executable projected scripts"
            );
        })
        .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn restart_recovers_xdg_metadata_without_swage_home() {
    GatewayProbe::without_explicit_root()
        .journey(async |probe| {
            assert_restart_recovers_without_swage_home(probe).await;
        })
        .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn restart_recovers_default_root_metadata_without_overrides() {
    GatewayProbe::with_default_root()
        .journey(async |probe| {
            assert_restart_recovers_without_swage_home(probe).await;
        })
        .await;
}

async fn assert_restart_recovers_without_swage_home(probe: &GatewayProbe) {
    probe.start().await;
    let first_pid = probe.pid();
    fs::remove_file(probe.metadata_path()).expect("failed to remove live runtime metadata");

    let port = probe.port.get().to_string();
    let output = probe
        .run(&["gateway", "restart", "--force", "--port", &port])
        .await;
    probe
        .assert_ready_after_start(output, Some(first_pid), probe.port.get())
        .await;

    let second_pid = probe.pid();
    assert_ne!(first_pid, second_pid);
    let environment = process_environment(second_pid);
    assert!(
        !environment
            .split(|byte| *byte == 0)
            .any(|entry| entry.starts_with(b"SWAGE_HOME=")),
        "replacement gateway gained an explicit SWAGE_HOME"
    );
    let expected_home = format!("HOME={}", probe.home.path().display());
    assert!(
        environment
            .split(|byte| *byte == 0)
            .any(|entry| entry == expected_home.as_bytes()),
        "replacement gateway did not receive recovered HOME"
    );
    let recovered_xdg = environment
        .split(|byte| *byte == 0)
        .find(|entry| entry.starts_with(b"XDG_RUNTIME_DIR="));
    if probe.xdg_runtime {
        let expected_xdg = format!("XDG_RUNTIME_DIR={}", probe.runtime.path().display());
        assert_eq!(recovered_xdg, Some(expected_xdg.as_bytes()));
    } else {
        assert_eq!(recovered_xdg, None);
    }
}

fn process_environment(pid: u32) -> Vec<u8> {
    fs::read(Path::new("/proc").join(pid.to_string()).join("environ"))
        .expect("failed to read the replacement process environment")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn restart_preserves_running_environment_and_still_starts_when_nothing_is_running() {
    GatewayProbe::new()
        .journey(async |probe| {
            probe.start().await;
            let first_pid = probe.pid();

            let invoking_runtime =
                tempfile::TempDir::new().expect("failed to create invoking runtime");
            let invoking_home = tempfile::TempDir::new().expect("failed to create invoking home");
            let invoking_metadata = invoking_runtime.path().join("swage/metadata.json");
            fs::create_dir_all(invoking_metadata.parent().unwrap())
                .expect("failed to create invoking metadata directory");
            // Keep the live metadata discoverable while giving the restart process a
            // conflicting runtime directory whose inherited value would fail below.
            fs::hard_link(probe.metadata_path(), &invoking_metadata)
                .expect("failed to expose runtime metadata to the restart command");

            let live_log = probe.root.path().join("gateway.log");
            fs::remove_file(&live_log).expect("failed to remove the initial gateway log");

            let port = "0";
            let mut restart = probe.command();
            let output = restart
                .env_remove("SWAGE_HOME")
                .env("HOME", invoking_home.path())
                .env("XDG_RUNTIME_DIR", invoking_runtime.path())
                .args(["gateway", "restart", "--force", "--port", port])
                .output()
                .await
                .expect("failed to invoke gateway restart");
            probe
                .assert_ready_after_start(output, Some(first_pid), 0)
                .await;

            let second_pid = probe.pid();
            assert_ne!(first_pid, second_pid);
            let environment = process_environment(second_pid);
            for (name, value) in [
                ("SWAGE_HOME", probe.root.path()),
                ("HOME", probe.home.path()),
                ("XDG_RUNTIME_DIR", probe.runtime.path()),
            ] {
                let expected = format!("{name}={}", value.display());
                assert!(
                    environment
                        .split(|byte| *byte == 0)
                        .any(|entry| entry == expected.as_bytes()),
                    "replacement gateway did not receive recovered {name}"
                );
            }
            assert!(live_log.exists());
            assert!(!invoking_metadata.exists());
            assert!(!invoking_home.path().join(".swage/gateway.log").exists());
            assert!(!invoking_home.path().join(".swage/swage.db").exists());

            probe.stop().await;
            assert!(!probe.metadata_path().exists());

            let output = probe
                .run(&["gateway", "restart", "--force", "--port", port])
                .await;
            probe.assert_ready_after_start(output, None, 0).await;
            assert!(probe.metadata_path().exists());
        })
        .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn foreground_restart_preserves_running_environment() {
    GatewayProbe::new().journey(async |probe| {
        probe.start().await;
        let first_pid = probe.pid();

        let invoking_runtime = tempfile::TempDir::new().expect("failed to create invoking runtime");
        let invoking_home = tempfile::TempDir::new().expect("failed to create invoking home");
        let invoking_metadata = invoking_runtime.path().join("swage/metadata.json");
        fs::create_dir_all(invoking_metadata.parent().unwrap())
            .expect("failed to create invoking metadata directory");
        fs::hard_link(probe.metadata_path(), &invoking_metadata)
            .expect("failed to expose runtime metadata to the restart command");

        let port = "0";
        let mut restart = probe.command()
            .kill_on_drop(true)
            .env_remove("SWAGE_HOME")
            .env("HOME", invoking_home.path())
            .env("XDG_RUNTIME_DIR", invoking_runtime.path())
            .args([
                "gateway",
                "restart",
                "--force",
                "--foreground",
                "--port",
                port,
            ])
            .spawn()
            .expect("failed to invoke foreground gateway restart");

        let second_pid = tokio::select! {
            pid = probe.wait_for_ready_process(Some(first_pid), 0) => pid,
            status = restart.wait() => panic!("foreground restart exited before readiness: {status:?}"),
        };
        let environment = process_environment(second_pid);
        for (name, value) in [
            ("SWAGE_HOME", probe.root.path()),
            ("HOME", probe.home.path()),
            ("XDG_RUNTIME_DIR", probe.runtime.path()),
        ] {
            let expected = format!("{name}={}", value.display());
            assert!(
                environment
                    .split(|byte| *byte == 0)
                    .any(|entry| entry == expected.as_bytes()),
                "foreground replacement gateway did not receive recovered {name}"
            );
        }
        assert!(!invoking_metadata.exists());

        let stop_output = probe.run(&["gateway", "stop", "--force"]).await;
        probe.assert_success(stop_output);
        let status = restart
            .wait()
            .await
            .expect("failed to wait for foreground gateway restart");
        assert!(status.success(), "foreground restart failed: {status}");
        assert!(!probe.metadata_path().exists());
    })
    .await;
}

fn watch_metadata(directory: &Path) -> AsyncFd<fs::File> {
    // SAFETY: inotify_init1 takes only flags and returns a new owned fd.
    let fd = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
    assert!(
        fd >= 0,
        "create metadata event fd: {}",
        std::io::Error::last_os_error()
    );
    // SAFETY: fd is valid, newly allocated and transferred exactly once.
    let file = unsafe { fs::File::from_raw_fd(fd) };
    let path = CString::new(directory.as_os_str().as_bytes()).expect("metadata directory path");
    // SAFETY: file owns a live inotify fd and path is NUL-terminated.
    let watch = unsafe {
        libc::inotify_add_watch(
            file.as_raw_fd(),
            path.as_ptr(),
            libc::IN_MOVED_TO | libc::IN_CLOSE_WRITE,
        )
    };
    assert!(
        watch >= 0,
        "watch metadata directory: {}",
        std::io::Error::last_os_error()
    );
    AsyncFd::new(file).expect("register metadata events with the test runtime")
}

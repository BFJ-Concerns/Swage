#![cfg(target_os = "linux")]

use std::{
    collections::hash_map::DefaultHasher,
    fs,
    hash::{Hash, Hasher},
    net::TcpListener,
    path::{Path, PathBuf},
    process::{Child, Command, Output},
    thread,
    time::{Duration, Instant},
};

use serde_json::Value;

const FIXTURE_READY_TIMEOUT: Duration = Duration::from_secs(30);

struct GatewayProbe {
    binary: PathBuf,
    root: tempfile::TempDir,
    runtime: tempfile::TempDir,
    home: tempfile::TempDir,
    port: u16,
    explicit_root: bool,
    xdg_runtime: bool,
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
        let mut hash = DefaultHasher::new();
        "qa-gateway-not-running-state".hash(&mut hash);
        root.path().hash(&mut hash);
        let offset = (hash.finish() % 1000) as u16;
        let listener = (0..1000)
            .find_map(|step| TcpListener::bind(("127.0.0.1", 35000 + (offset + step) % 1000)).ok())
            .expect("failed to reserve a loopback port for the gateway probe");
        let port = listener
            .local_addr()
            .expect("failed to read the reserved loopback address")
            .port();
        drop(listener);
        Self {
            binary: PathBuf::from(env!("CARGO_BIN_EXE_swage")),
            root,
            // Keep the runtime path short enough for swage-sandbox's UUID socket
            // name, so it never falls back outside the scratch filesystem.
            runtime: tempfile::Builder::new()
                .prefix("qa-gateway-not-running-state-")
                .rand_bytes(1)
                .tempdir_in("/tmp/scratch")
                .expect("failed to create the probe runtime dir"),
            home: tempfile::TempDir::new().expect("failed to create the probe home"),
            port,
            explicit_root,
            xdg_runtime,
        }
    }

    fn command(&self) -> Command {
        let mut command = Command::new("setsid");
        command.arg(&self.binary);
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

    fn run(&self, arguments: &[&str]) -> Output {
        self.command()
            .args(arguments)
            .output()
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

    fn assert_ready_after_start(&self, output: Output) {
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success() || stderr.contains("gateway did not become ready within 5s"),
            "gateway start failed before readiness polling\nstdout: {}\nstderr: {stderr}",
            String::from_utf8_lossy(&output.stdout),
        );

        // The CLI's five-second deadline is part of its reporting contract,
        // not a startup-time guarantee. These tests exercise restart state,
        // so let a slow fixture finish starting and wait on the real health
        // condition instead of treating scheduler pressure as a failed setup.
        let deadline = Instant::now() + FIXTURE_READY_TIMEOUT;
        loop {
            if self.is_healthy() {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "gateway did not become healthy after the start command timed out"
            );
            thread::sleep(Duration::from_millis(25));
        }
    }

    fn is_healthy(&self) -> bool {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("failed to build the health-check runtime")
            .block_on(
                swage_client::http::GatewayClient::new(&format!("http://127.0.0.1:{}", self.port))
                    .health(),
            )
            .is_ok()
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

    fn start(&self) {
        let port = self.port.to_string();
        self.assert_ready_after_start(self.run(&["gateway", "start", "--port", &port]));
    }

    fn stop(&self) {
        let _ = self.run(&["gateway", "stop", "--force"]);
    }
}

impl Drop for GatewayProbe {
    fn drop(&mut self) {
        self.stop();
    }
}

#[test]
fn clean_stop_status_reports_not_running_without_an_error() {
    let probe = GatewayProbe::new();
    probe.start();
    probe.assert_success(probe.run(&["gateway", "stop"]));
    assert!(!probe.metadata_path().exists());

    let output = probe.run(&["gateway", "status"]);
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
}

struct ForegroundGateway(Child);

impl Drop for ForegroundGateway {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn start_after_sigkill_recovers_without_a_status_call() {
    let probe = GatewayProbe::new();
    let port = probe.port.to_string();
    let mut child = ForegroundGateway(
        probe
            .command()
            .args(["gateway", "start", "--foreground", "--port", &port])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("failed to start foreground gateway"),
    );
    let deadline = Instant::now() + FIXTURE_READY_TIMEOUT;
    loop {
        if probe.metadata_path().exists() && probe.is_healthy() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "foreground gateway did not become healthy"
        );
        thread::sleep(Duration::from_millis(25));
    }
    let killed_pid = probe.pid();
    assert_eq!(killed_pid, child.0.id());
    child.0.kill().expect("failed to SIGKILL owned gateway");

    // Do not wait/reap or call status: a terminated child retains its /proc
    // entry until the parent reaps it, just as an unreaped detached daemon can.
    let deadline = Instant::now() + FIXTURE_READY_TIMEOUT;
    loop {
        let status = fs::read_to_string(format!("/proc/{killed_pid}/status"))
            .expect("unreaped gateway should still have a process entry");
        if let Some(state) = status.lines().find(|line| line.starts_with("State:"))
            && state.contains('Z')
        {
            eprintln!("Killed gateway {state}");
            break;
        }
        assert!(
            Instant::now() < deadline,
            "killed gateway did not terminate"
        );
        thread::sleep(Duration::from_millis(1));
    }
    assert!(
        !probe.is_healthy(),
        "killed gateway must no longer serve health"
    );
    let output = probe.run(&["gateway", "start", "--port", &port]);
    probe.assert_ready_after_start(output);
    assert_ne!(
        probe.pid(),
        killed_pid,
        "start must publish a replacement PID"
    );
    assert!(probe.is_healthy(), "replacement gateway must serve health");
}

#[cfg(unix)]
#[test]
fn gateway_start_regenerates_fresh_and_damaged_shipped_skill_projection() {
    use std::os::unix::fs::PermissionsExt;

    let probe = GatewayProbe::new();
    let projected = probe
        .root
        .path()
        .join("generated/shipped-skills/commission/scripts/create-annexe.sh");
    let projection_root = probe.root.path().join("generated/shipped-skills");
    let expected = include_str!("../../../../shipped/skills/commission/scripts/create-annexe.sh");

    probe.start();
    assert_eq!(
        fs::read(&projected).expect("gateway startup projected shipped skill"),
        expected.as_bytes(),
        "the gateway startup consumer must project bundled skill bytes into a fresh root"
    );
    probe.stop();

    fs::write(&projected, "damaged projection").expect("damage projected skill");
    fs::set_permissions(&projected, fs::Permissions::from_mode(0o600))
        .expect("remove projected script execute mode");
    let stray = projection_root.join("stray.txt");
    fs::write(&stray, "stray").expect("write stray projection file");

    probe.start();
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
}

#[test]
fn restart_recovers_xdg_metadata_without_swage_home() {
    assert_restart_recovers_without_swage_home(GatewayProbe::without_explicit_root());
}

#[test]
fn restart_recovers_default_root_metadata_without_overrides() {
    assert_restart_recovers_without_swage_home(GatewayProbe::with_default_root());
}

fn assert_restart_recovers_without_swage_home(probe: GatewayProbe) {
    probe.start();
    let first_pid = probe.pid();
    fs::remove_file(probe.metadata_path()).expect("failed to remove live runtime metadata");

    let port = probe.port.to_string();
    let output = probe.run(&["gateway", "restart", "--force", "--port", &port]);
    probe.assert_ready_after_start(output);

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

fn wait_for_replacement_pid(probe: &GatewayProbe, previous_pid: u32, child: &mut Child) -> u32 {
    let deadline = Instant::now() + FIXTURE_READY_TIMEOUT;
    loop {
        if let Ok(bytes) = fs::read(probe.metadata_path())
            && let Ok(metadata) = serde_json::from_slice::<Value>(&bytes)
            && let Some(pid) = metadata["pid"].as_u64()
            && pid as u32 != previous_pid
        {
            return pid as u32;
        }
        if let Some(status) = child
            .try_wait()
            .expect("failed to inspect the foreground restart process")
        {
            panic!("foreground restart exited before replacing the gateway: {status}");
        }
        assert!(
            Instant::now() < deadline,
            "foreground replacement did not publish runtime metadata"
        );
        thread::sleep(Duration::from_millis(25));
    }
}

#[test]
fn restart_preserves_running_environment_and_still_starts_when_nothing_is_running() {
    let probe = GatewayProbe::new();
    probe.start();
    let first_pid = probe.pid();

    let invoking_runtime = tempfile::TempDir::new().expect("failed to create invoking runtime");
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

    let port = probe.port.to_string();
    let mut restart = probe.command();
    let output = restart
        .env_remove("SWAGE_HOME")
        .env("HOME", invoking_home.path())
        .env("XDG_RUNTIME_DIR", invoking_runtime.path())
        .args(["gateway", "restart", "--force", "--port", &port])
        .output()
        .expect("failed to invoke gateway restart");
    probe.assert_ready_after_start(output);

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

    probe.stop();
    assert!(!probe.metadata_path().exists());

    let output = probe.run(&["gateway", "restart", "--force", "--port", &port]);
    probe.assert_ready_after_start(output);
    assert!(probe.metadata_path().exists());
}

#[test]
fn foreground_restart_preserves_running_environment() {
    let probe = GatewayProbe::new();
    probe.start();
    let first_pid = probe.pid();

    let invoking_runtime = tempfile::TempDir::new().expect("failed to create invoking runtime");
    let invoking_home = tempfile::TempDir::new().expect("failed to create invoking home");
    let invoking_metadata = invoking_runtime.path().join("swage/metadata.json");
    fs::create_dir_all(invoking_metadata.parent().unwrap())
        .expect("failed to create invoking metadata directory");
    fs::hard_link(probe.metadata_path(), &invoking_metadata)
        .expect("failed to expose runtime metadata to the restart command");

    let port = probe.port.to_string();
    let mut restart = probe
        .command()
        .env_remove("SWAGE_HOME")
        .env("HOME", invoking_home.path())
        .env("XDG_RUNTIME_DIR", invoking_runtime.path())
        .args([
            "gateway",
            "restart",
            "--force",
            "--foreground",
            "--port",
            &port,
        ])
        .spawn()
        .expect("failed to invoke foreground gateway restart");

    let second_pid = wait_for_replacement_pid(&probe, first_pid, &mut restart);
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

    let stop_output = probe.run(&["gateway", "stop", "--force"]);
    probe.assert_success(stop_output);
    let status = restart
        .wait()
        .expect("failed to wait for foreground gateway restart");
    assert!(status.success(), "foreground restart failed: {status}");
    assert!(!probe.metadata_path().exists());
}

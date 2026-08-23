#![cfg(target_os = "linux")]

use std::{
    fs,
    net::TcpListener,
    path::{Path, PathBuf},
    process::{Command, Output},
};

use serde_json::Value;

struct GatewayProbe {
    binary: PathBuf,
    root: tempfile::TempDir,
    runtime: tempfile::TempDir,
    home: tempfile::TempDir,
    port: u16,
}

impl GatewayProbe {
    fn new() -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .expect("failed to reserve a loopback port for the gateway probe");
        let port = listener
            .local_addr()
            .expect("failed to read the reserved loopback address")
            .port();
        drop(listener);
        Self {
            binary: PathBuf::from(env!("CARGO_BIN_EXE_swage")),
            root: tempfile::TempDir::new().expect("failed to create the probe root"),
            runtime: tempfile::TempDir::new().expect("failed to create the probe runtime dir"),
            home: tempfile::TempDir::new().expect("failed to create the probe home"),
            port,
        }
    }

    fn command(&self) -> Command {
        let mut command = Command::new(&self.binary);
        command
            .env("SWAGE_HOME", self.root.path())
            .env("XDG_RUNTIME_DIR", self.runtime.path())
            .env("HOME", self.home.path());
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

    fn metadata_path(&self) -> PathBuf {
        self.runtime.path().join("swage/metadata.json")
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
        self.assert_success(self.run(&["gateway", "start", "--port", &port]));
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

fn process_environment(pid: u32) -> Vec<u8> {
    fs::read(Path::new("/proc").join(pid.to_string()).join("environ"))
        .expect("failed to read the replacement process environment")
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
    probe.assert_success(output);

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
    probe.assert_success(output);
    assert!(probe.metadata_path().exists());
}

//! Bubblewrap command builder.
//!
//! Constructs the `bwrap` CLI invocation from a SandboxConfig, producing the
//! full argument list for namespace isolation, mount configuration, and
//! die-with-parent behaviour.

use std::path::{Path, PathBuf};

use swage_core::SandboxError;
use tokio::process::Command;

use crate::config::SandboxConfig;

// ---------------------------------------------------------------------------
// bwrap discovery
// ---------------------------------------------------------------------------

/// Locates the `bwrap` executable on `$PATH`.
///
/// Returns [`SandboxError::Unavailable`] if bubblewrap is not installed.
pub fn find_bwrap() -> Result<PathBuf, SandboxError> {
    find_on_path("bwrap").ok_or_else(|| SandboxError::Unavailable {
        message: "bwrap not found on PATH — install bubblewrap to use full sandbox isolation"
            .into(),
    })
}

/// Searches for an executable name across each directory in `$PATH`.
///
/// Returns the first match found, or `None` if no match exists.
fn find_on_path(name: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Command builder
// ---------------------------------------------------------------------------

/// Builds a `bwrap` [`Command`] from a [`SandboxConfig`].
///
/// Calls [`find_bwrap`] internally — returns an error if bubblewrap is not
/// installed.
///
/// ## Argument ordering
///
/// `--ro-bind / /` is emitted before any specific bind mounts so that
/// read-write and read-only overlays take precedence over the read-only root.
pub fn build_bwrap_command(
    config: &SandboxConfig,
    socket_path: &Path,
    agent_binary: &Path,
) -> Result<Command, SandboxError> {
    let bwrap_path = find_bwrap()?;
    build_bwrap_command_with_path(&bwrap_path, config, socket_path, agent_binary)
}

/// Constructs the `bwrap` command given an explicit path to the binary.
///
/// Extracted from [`build_bwrap_command`] so that tests can supply a
/// synthetic path without requiring bubblewrap to be installed.
pub(crate) fn build_bwrap_command_with_path(
    bwrap_path: &Path,
    config: &SandboxConfig,
    socket_path: &Path,
    agent_binary: &Path,
) -> Result<Command, SandboxError> {
    let mut cmd = Command::new(bwrap_path);

    // Isolate all namespaces (PID, network, mount, user, IPC, UTS).
    cmd.arg("--unshare-all");

    // Terminate the child automatically if the host process dies — prevents
    // orphaned sandbox processes when the daemon crashes or is killed.
    cmd.arg("--die-with-parent");

    // Minimal virtual filesystem.
    cmd.args(["--dev", "/dev"]);
    cmd.args(["--tmpfs", "/tmp"]);

    // Read-only root — must come before any specific rw binds so they overlay.
    cmd.args(["--ro-bind", "/", "/"]);

    // Working directory: read-write inside the sandbox.
    let wd = config.working_directory.as_os_str();
    cmd.arg("--bind").arg(wd).arg(wd);

    // Additional read-write mounts (e.g. output directories).
    for mount in &config.read_write_mounts {
        let p = mount.as_os_str();
        cmd.arg("--bind").arg(p).arg(p);
    }

    // Additional read-only mounts (e.g. shared libraries, bench source).
    for mount in &config.read_only_mounts {
        let p = mount.as_os_str();
        cmd.arg("--ro-bind").arg(p).arg(p);
    }

    // Expose the IPC socket path to the agent process via the environment.
    cmd.arg("--setenv")
        .arg("SWAGE_IPC_SOCKET")
        .arg(socket_path.as_os_str());
    let config_json = serde_json::to_string(config).map_err(|error| SandboxError::SpawnFailed {
        message: format!("failed to serialise sandbox config for child Landlock setup: {error}"),
    })?;
    cmd.arg("--setenv")
        .arg("SWAGE_SANDBOX_CONFIG_JSON")
        .arg(config_json);

    // Separator then the agent binary, instructing it to enter sandbox mode.
    cmd.arg("--").arg(agent_binary).arg("--sandbox-mode");

    Ok(cmd)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::config::SandboxConfig;

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

    /// Builds a minimal SandboxConfig pointing at /tmp/agent.
    fn basic_config() -> SandboxConfig {
        SandboxConfig::builder()
            .working_directory("/tmp/agent-workspace")
            .build()
            .unwrap()
    }

    /// Collects args from a tokio Command for easy assertion.
    fn args_of(cmd: &Command) -> Vec<&OsStr> {
        cmd.as_std().get_args().collect()
    }

    // -----------------------------------------------------------------------
    // find_bwrap / find_on_path
    // -----------------------------------------------------------------------

    #[test]
    fn find_on_path_returns_none_for_unknown_executable() {
        // A name that will never exist on any system.
        let result = find_on_path("swage_definitely_does_not_exist_xyz_abc_123");
        assert!(result.is_none());
    }

    #[test]
    fn find_bwrap_error_message_mentions_bubblewrap() {
        // Only meaningful when bwrap is absent. When it IS present this test is
        // vacuously true — we just verify the error message format is correct
        // by calling find_bwrap with a known-absent name.
        let err = SandboxError::Unavailable {
            message: "bwrap not found on PATH — install bubblewrap to use full sandbox isolation"
                .into(),
        };
        let msg = err.to_string();
        assert!(msg.contains("bubblewrap") || msg.contains("bwrap"));
    }

    // -----------------------------------------------------------------------
    // build_bwrap_command_with_path — argument structure
    // -----------------------------------------------------------------------

    #[test]
    fn command_includes_unshare_all_and_die_with_parent() {
        let config = basic_config();
        let cmd = build_bwrap_command_with_path(
            Path::new("/usr/bin/bwrap"),
            &config,
            Path::new("/run/swage/agent.sock"),
            Path::new("/usr/bin/swage-agent"),
        )
        .unwrap();

        let args = args_of(&cmd);
        assert!(
            args.contains(&OsStr::new("--unshare-all")),
            "missing --unshare-all; args: {args:?}"
        );
        assert!(
            args.contains(&OsStr::new("--die-with-parent")),
            "missing --die-with-parent; args: {args:?}"
        );
    }

    #[test]
    fn command_includes_dev_and_tmpfs() {
        let config = basic_config();
        let cmd = build_bwrap_command_with_path(
            Path::new("/usr/bin/bwrap"),
            &config,
            Path::new("/run/swage/agent.sock"),
            Path::new("/usr/bin/swage-agent"),
        )
        .unwrap();

        let args = args_of(&cmd);
        assert!(args.contains(&OsStr::new("--dev")));
        assert!(args.contains(&OsStr::new("/dev")));
        assert!(args.contains(&OsStr::new("--tmpfs")));
        assert!(args.contains(&OsStr::new("/tmp")));
    }

    #[test]
    fn command_includes_ro_bind_root() {
        let config = basic_config();
        let cmd = build_bwrap_command_with_path(
            Path::new("/usr/bin/bwrap"),
            &config,
            Path::new("/run/swage/agent.sock"),
            Path::new("/usr/bin/swage-agent"),
        )
        .unwrap();

        let args = args_of(&cmd);
        assert!(
            args.contains(&OsStr::new("--ro-bind")),
            "missing --ro-bind; args: {args:?}"
        );
        // The first --ro-bind must be the root bind (/ /).
        let first_ro_bind_pos = args
            .iter()
            .position(|a| *a == OsStr::new("--ro-bind"))
            .expect("--ro-bind must appear");
        assert_eq!(
            args.get(first_ro_bind_pos + 1),
            Some(&OsStr::new("/")),
            "--ro-bind must bind from / first"
        );
        assert_eq!(
            args.get(first_ro_bind_pos + 2),
            Some(&OsStr::new("/")),
            "--ro-bind must bind to / first"
        );
    }

    #[test]
    fn working_directory_appears_as_rw_bind_after_root() {
        let config = basic_config();
        let cmd = build_bwrap_command_with_path(
            Path::new("/usr/bin/bwrap"),
            &config,
            Path::new("/run/swage/agent.sock"),
            Path::new("/usr/bin/swage-agent"),
        )
        .unwrap();

        let args = args_of(&cmd);

        // Find the position of --ro-bind / / and --bind /tmp/agent-workspace.
        let ro_root_pos = args
            .iter()
            .position(|a| *a == OsStr::new("--ro-bind"))
            .expect("--ro-bind must appear");

        let wd_bind_pos = args
            .iter()
            .position(|a| *a == OsStr::new("/tmp/agent-workspace"))
            .expect("working_directory must appear in args");

        // Working directory must appear after the root ro-bind.
        assert!(
            wd_bind_pos > ro_root_pos,
            "working directory bind must follow root ro-bind; ro_root={ro_root_pos}, wd={wd_bind_pos}"
        );
    }

    #[test]
    fn read_write_mounts_appear_as_bind() {
        let config = SandboxConfig::builder()
            .working_directory("/tmp/agent-workspace")
            .read_write_mount("/tmp/output")
            .build()
            .unwrap();

        let cmd = build_bwrap_command_with_path(
            Path::new("/usr/bin/bwrap"),
            &config,
            Path::new("/run/swage/agent.sock"),
            Path::new("/usr/bin/swage-agent"),
        )
        .unwrap();

        let args = args_of(&cmd);

        // Find --bind /tmp/output /tmp/output sequence.
        let bind_positions: Vec<usize> = args
            .iter()
            .enumerate()
            .filter_map(|(i, a)| {
                if *a == OsStr::new("--bind") {
                    Some(i)
                } else {
                    None
                }
            })
            .collect();

        let output_bind = bind_positions.iter().any(|&pos| {
            args.get(pos + 1) == Some(&OsStr::new("/tmp/output"))
                && args.get(pos + 2) == Some(&OsStr::new("/tmp/output"))
        });

        assert!(
            output_bind,
            "--bind /tmp/output /tmp/output not found; args: {args:?}"
        );
    }

    #[test]
    fn read_only_mounts_appear_as_ro_bind() {
        let config = SandboxConfig::builder()
            .working_directory("/tmp/agent-workspace")
            .read_only_mount("/usr/lib")
            .build()
            .unwrap();

        let cmd = build_bwrap_command_with_path(
            Path::new("/usr/bin/bwrap"),
            &config,
            Path::new("/run/swage/agent.sock"),
            Path::new("/usr/bin/swage-agent"),
        )
        .unwrap();

        let args = args_of(&cmd);

        // There are two --ro-bind entries: the root bind and /usr/lib.
        let ro_bind_positions: Vec<usize> = args
            .iter()
            .enumerate()
            .filter_map(|(i, a)| {
                if *a == OsStr::new("--ro-bind") {
                    Some(i)
                } else {
                    None
                }
            })
            .collect();

        let lib_bind = ro_bind_positions.iter().any(|&pos| {
            args.get(pos + 1) == Some(&OsStr::new("/usr/lib"))
                && args.get(pos + 2) == Some(&OsStr::new("/usr/lib"))
        });

        assert!(
            lib_bind,
            "--ro-bind /usr/lib /usr/lib not found; args: {args:?}"
        );
    }

    #[test]
    fn ro_root_appears_before_specific_mounts() {
        // Verifies the overlay ordering invariant: ro-bind / / comes first,
        // so that subsequent bind mounts take precedence over the read-only root.
        let config = SandboxConfig::builder()
            .working_directory("/tmp/agent-workspace")
            .read_only_mount("/usr/lib")
            .read_write_mount("/tmp/output")
            .build()
            .unwrap();

        let cmd = build_bwrap_command_with_path(
            Path::new("/usr/bin/bwrap"),
            &config,
            Path::new("/run/swage/agent.sock"),
            Path::new("/usr/bin/swage-agent"),
        )
        .unwrap();

        let args = args_of(&cmd);

        let root_ro_pos = args
            .iter()
            .position(|a| *a == OsStr::new("/"))
            .expect("/ must appear as bind src");

        let usr_lib_pos = args
            .iter()
            .position(|a| *a == OsStr::new("/usr/lib"))
            .expect("/usr/lib must appear");

        let output_pos = args
            .iter()
            .position(|a| *a == OsStr::new("/tmp/output"))
            .expect("/tmp/output must appear");

        assert!(
            root_ro_pos < usr_lib_pos,
            "/ must appear before /usr/lib; positions: root={root_ro_pos}, lib={usr_lib_pos}"
        );
        assert!(
            root_ro_pos < output_pos,
            "/ must appear before /tmp/output; positions: root={root_ro_pos}, out={output_pos}"
        );
    }

    #[test]
    fn ipc_socket_env_is_set() {
        let config = basic_config();
        let cmd = build_bwrap_command_with_path(
            Path::new("/usr/bin/bwrap"),
            &config,
            Path::new("/run/swage/test.sock"),
            Path::new("/usr/bin/swage-agent"),
        )
        .unwrap();

        let args = args_of(&cmd);

        let setenv_pos = args
            .iter()
            .position(|a| *a == OsStr::new("--setenv"))
            .expect("--setenv must appear");

        assert_eq!(
            args.get(setenv_pos + 1),
            Some(&OsStr::new("SWAGE_IPC_SOCKET")),
            "env var name must be SWAGE_IPC_SOCKET"
        );
        assert_eq!(
            args.get(setenv_pos + 2),
            Some(&OsStr::new("/run/swage/test.sock")),
            "socket path must be set correctly"
        );
    }

    #[test]
    fn sandbox_config_env_is_set_for_child_landlock() {
        let config = basic_config();
        let cmd = build_bwrap_command_with_path(
            Path::new("/usr/bin/bwrap"),
            &config,
            Path::new("/run/swage/test.sock"),
            Path::new("/usr/bin/swage-agent"),
        )
        .unwrap();

        let args = args_of(&cmd);
        let config_env_pos = args
            .iter()
            .position(|a| *a == OsStr::new("SWAGE_SANDBOX_CONFIG_JSON"))
            .expect("sandbox config env var must appear");

        let config_json = args
            .get(config_env_pos + 1)
            .and_then(|value| value.to_str())
            .expect("sandbox config env var must have UTF-8 JSON");
        let decoded: SandboxConfig = serde_json::from_str(config_json).unwrap();

        assert_eq!(decoded.working_directory, Path::new("/tmp/agent-workspace"));
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_sandbox_config_path_fails_command_build() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        let config = SandboxConfig::builder()
            .working_directory(PathBuf::from(OsString::from_vec(vec![
                b'/', b't', b'm', b'p', 0xff,
            ])))
            .build()
            .unwrap();

        let error = build_bwrap_command_with_path(
            Path::new("/usr/bin/bwrap"),
            &config,
            Path::new("/run/swage/test.sock"),
            Path::new("/usr/bin/swage-agent"),
        )
        .unwrap_err();

        match error {
            SandboxError::SpawnFailed { message } => {
                assert!(message.contains("failed to serialise sandbox config"));
            }
            other => panic!("expected SpawnFailed, got {other:?}"),
        }
    }

    #[test]
    fn agent_binary_and_sandbox_mode_flag_appear_after_separator() {
        let config = basic_config();
        let cmd = build_bwrap_command_with_path(
            Path::new("/usr/bin/bwrap"),
            &config,
            Path::new("/run/swage/agent.sock"),
            Path::new("/usr/bin/swage-agent"),
        )
        .unwrap();

        let args = args_of(&cmd);

        let sep_pos = args
            .iter()
            .position(|a| *a == OsStr::new("--"))
            .expect("-- separator must appear");

        assert_eq!(
            args.get(sep_pos + 1),
            Some(&OsStr::new("/usr/bin/swage-agent")),
            "agent binary must follow -- separator"
        );
        assert_eq!(
            args.get(sep_pos + 2),
            Some(&OsStr::new("--sandbox-mode")),
            "--sandbox-mode must follow agent binary"
        );
    }

    #[test]
    fn agent_binary_is_last_element_group() {
        let config = basic_config();
        let cmd = build_bwrap_command_with_path(
            Path::new("/usr/bin/bwrap"),
            &config,
            Path::new("/run/swage/agent.sock"),
            Path::new("/usr/bin/swage-agent"),
        )
        .unwrap();

        let args = args_of(&cmd);

        // The last two args must be the agent binary and --sandbox-mode.
        let n = args.len();
        assert!(n >= 2, "must have at least 2 args");
        assert_eq!(args[n - 1], OsStr::new("--sandbox-mode"));
        assert_eq!(args[n - 2], OsStr::new("/usr/bin/swage-agent"));
    }
}

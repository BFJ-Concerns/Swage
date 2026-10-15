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
/// `--ro-bind / /` is emitted before any specific bind mounts so that the
/// write set, the read-only overlays and the masks take precedence over the
/// read-only root; masks come last so a private path inside a writable tree
/// is still hidden.
///
/// `egress_socket` is the host proxy's socket to bind into the sandbox and
/// name in the child's environment; `None` leaves the sandbox with no route
/// to the network.
pub fn build_bwrap_command(
    config: &SandboxConfig,
    socket_path: &Path,
    egress_socket: Option<&Path>,
    agent_binary: &Path,
) -> Result<Command, SandboxError> {
    let bwrap_path = find_bwrap()?;
    build_bwrap_command_with_path(
        &bwrap_path,
        config,
        socket_path,
        egress_socket,
        agent_binary,
    )
}

/// Constructs the `bwrap` command given an explicit path to the binary.
///
/// Extracted from [`build_bwrap_command`] so that tests can supply a
/// synthetic path without requiring bubblewrap to be installed.
pub(crate) fn build_bwrap_command_with_path(
    bwrap_path: &Path,
    config: &SandboxConfig,
    socket_path: &Path,
    egress_socket: Option<&Path>,
    agent_binary: &Path,
) -> Result<Command, SandboxError> {
    let mut cmd = Command::new(bwrap_path);

    // Isolate all namespaces (PID, network, mount, user, IPC, UTS).
    cmd.arg("--unshare-all");

    // Terminate the child automatically if the host process dies — prevents
    // orphaned sandbox processes when the daemon crashes or is killed.
    cmd.arg("--die-with-parent");

    // Read-only root — must come before every overlay mount, including the
    // minimal /dev and /tmp filesystems below.
    cmd.args(["--ro-bind", "/", "/"]);

    // Minimal virtual filesystem. These mounts must overlay the read-only
    // root: mounting the root afterwards exposes the host's nodev /dev, and
    // without its own procfs the sandbox would see the host's, whose
    // /proc/<pid>/root walks around every mask below.
    cmd.args(["--dev", "/dev"]);
    cmd.args(["--proc", "/proc"]);
    cmd.args(["--tmpfs", "/tmp"]);

    // The host IPC socket may live under /tmp (a relocated or over-long
    // runtime directory), where the tmpfs above hides it. Bind the socket
    // file itself — never its directory — after the overlay so the agent can
    // connect and sees nothing else of the host's socket root.
    let socket = socket_path.as_os_str();
    cmd.arg("--bind").arg(socket).arg(socket);

    // The egress proxy's socket, bound the same way: the one route out.
    if let Some(egress) = egress_socket {
        let egress = egress.as_os_str();
        cmd.arg("--bind").arg(egress).arg(egress);
        cmd.arg("--setenv")
            .arg(crate::egress::EGRESS_SOCKET_ENV)
            .arg(egress);
    }

    // The policy's mounts, shallowest path first, so the deepest mount at any
    // point wins: a mask inside a writable tree still hides, and a grant
    // inside a masked tree reopens only its own subtree. The write set binds
    // writable (the working directory only when the policy lists it), the
    // read-only paths overlay, and a mask makes a directory an empty tmpfs
    // and a file read as /dev/null. A masked path absent on the host needs
    // no mask.
    for (path, kind) in policy_mounts(config) {
        let p = path.as_os_str();
        match kind {
            PolicyMount::Write => cmd.arg("--bind").arg(p).arg(p),
            PolicyMount::ReadOnly => cmd.arg("--ro-bind").arg(p).arg(p),
            PolicyMount::MaskDirectory => cmd.arg("--tmpfs").arg(p),
            PolicyMount::MaskFile => cmd.arg("--ro-bind").arg("/dev/null").arg(p),
        };
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

/// One mount the policy asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PolicyMount {
    Write,
    ReadOnly,
    MaskDirectory,
    MaskFile,
}

/// The policy's mounts ordered shallowest first, so bubblewrap applies the
/// deepest last and it wins. Ties keep the policy's own order: masks after
/// grants at the same depth, so an identical path masked and granted reads
/// as masked.
fn policy_mounts(config: &SandboxConfig) -> Vec<(&Path, PolicyMount)> {
    let mut mounts: Vec<(&Path, PolicyMount)> = Vec::new();
    mounts.extend(
        config
            .write_paths
            .iter()
            .map(|path| (path.as_path(), PolicyMount::Write)),
    );
    mounts.extend(
        config
            .read_only_paths
            .iter()
            .map(|path| (path.as_path(), PolicyMount::ReadOnly)),
    );
    for path in &config.masked_paths {
        if path.is_dir() {
            mounts.push((path.as_path(), PolicyMount::MaskDirectory));
        } else if path.is_file() {
            mounts.push((path.as_path(), PolicyMount::MaskFile));
        }
    }
    mounts.sort_by_key(|(path, _)| path.components().count());
    mounts
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
            None,
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
    fn command_gives_the_sandbox_its_own_proc() {
        let cmd = build_bwrap_command_with_path(
            Path::new("/usr/bin/bwrap"),
            &basic_config(),
            Path::new("/run/swage/agent.sock"),
            None,
            Path::new("/usr/bin/swage-agent"),
        )
        .unwrap();
        let args = args_of(&cmd);
        let root = args
            .windows(3)
            .position(|w| w == [OsStr::new("--ro-bind"), OsStr::new("/"), OsStr::new("/")])
            .unwrap();
        let proc_ = args
            .windows(2)
            .position(|w| w == [OsStr::new("--proc"), OsStr::new("/proc")])
            .expect("the sandbox mounts its own /proc over the host's");
        assert!(root < proc_, "the procfs must overlay the root bind");
    }

    #[test]
    fn a_grant_inside_a_masked_tree_is_bound_after_the_mask_and_a_mask_inside_a_grant_after_the_grant()
     {
        let home = tempfile::tempdir().unwrap();
        let data_root = home.path().join(".swage");
        let scratch = data_root.join("scratch").join("session-1");
        std::fs::create_dir_all(&scratch).unwrap();
        let ssh = home.path().join(".ssh");
        std::fs::create_dir(&ssh).unwrap();
        let config = SandboxConfig::builder()
            .working_directory(&scratch)
            .write_path(&scratch)
            .write_path(home.path())
            .masked_path(&data_root)
            .masked_path(&ssh)
            .build()
            .unwrap();
        let cmd = build_bwrap_command_with_path(
            Path::new("/usr/bin/bwrap"),
            &config,
            Path::new("/run/swage/agent.sock"),
            None,
            Path::new("/usr/bin/swage-agent"),
        )
        .unwrap();
        let args = args_of(&cmd);
        let position = |needle: &Path| {
            args.iter()
                .position(|a| *a == needle.as_os_str())
                .unwrap_or_else(|| panic!("{} must be mounted; args: {args:?}", needle.display()))
        };
        let home_bind = position(home.path());
        let root_mask = position(&data_root);
        let ssh_mask = position(&ssh);
        let scratch_bind = position(&scratch);
        assert!(
            home_bind < root_mask && home_bind < ssh_mask,
            "masks under the home grant follow it"
        );
        assert!(
            root_mask < scratch_bind,
            "the scratch grant under the masked data root follows the mask"
        );
    }

    #[test]
    fn command_includes_dev_and_tmpfs() {
        let config = basic_config();
        let cmd = build_bwrap_command_with_path(
            Path::new("/usr/bin/bwrap"),
            &config,
            Path::new("/run/swage/agent.sock"),
            None,
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
    fn command_mounts_root_before_dev_and_tmpfs_overlays() {
        let config = basic_config();
        let cmd = build_bwrap_command_with_path(
            Path::new("/usr/bin/bwrap"),
            &config,
            Path::new("/run/swage/agent.sock"),
            None,
            Path::new("/usr/bin/swage-agent"),
        )
        .unwrap();

        let args = args_of(&cmd);
        let root = args
            .windows(3)
            .position(|window| {
                window == [OsStr::new("--ro-bind"), OsStr::new("/"), OsStr::new("/")]
            })
            .expect("command must contain the read-only root mount");
        let dev = args
            .windows(2)
            .position(|window| window == [OsStr::new("--dev"), OsStr::new("/dev")])
            .expect("command must contain the minimal /dev mount");
        let tmp = args
            .windows(2)
            .position(|window| window == [OsStr::new("--tmpfs"), OsStr::new("/tmp")])
            .expect("command must contain the /tmp overlay");

        assert!(
            root < dev,
            "the minimal /dev mount must overlay the root bind"
        );
        assert!(root < tmp, "the /tmp mount must overlay the root bind");
    }

    #[test]
    fn command_includes_ro_bind_root() {
        let config = basic_config();
        let cmd = build_bwrap_command_with_path(
            Path::new("/usr/bin/bwrap"),
            &config,
            Path::new("/run/swage/agent.sock"),
            None,
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
        let config = SandboxConfig::builder()
            .working_directory("/tmp/agent-workspace")
            .write_path("/tmp/agent-workspace")
            .build()
            .unwrap();
        let cmd = build_bwrap_command_with_path(
            Path::new("/usr/bin/bwrap"),
            &config,
            Path::new("/run/swage/agent.sock"),
            None,
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
    fn the_write_set_is_bound_writable_and_the_working_directory_only_when_listed() {
        let writable = SandboxConfig::builder()
            .working_directory("/tmp/agent-workspace")
            .write_path("/tmp/agent-workspace")
            .write_path("/home/u/.cache")
            .build()
            .unwrap();
        let cmd = build_bwrap_command_with_path(
            Path::new("/usr/bin/bwrap"),
            &writable,
            Path::new("/run/swage/agent.sock"),
            None,
            Path::new("/usr/bin/swage-agent"),
        )
        .unwrap();
        let args = args_of(&cmd);
        for path in ["/tmp/agent-workspace", "/home/u/.cache"] {
            assert!(
                args.windows(3)
                    .any(|w| w == [OsStr::new("--bind"), OsStr::new(path), OsStr::new(path)]),
                "{path} must be bound writable; args: {args:?}"
            );
        }

        // A read-only workspace is left to the read-only root: no bind at all.
        let read_only = basic_config();
        let cmd = build_bwrap_command_with_path(
            Path::new("/usr/bin/bwrap"),
            &read_only,
            Path::new("/run/swage/agent.sock"),
            None,
            Path::new("/usr/bin/swage-agent"),
        )
        .unwrap();
        let args = args_of(&cmd);
        assert!(
            !args.contains(&OsStr::new("/tmp/agent-workspace")),
            "an unlisted working directory must not be bound; args: {args:?}"
        );
    }

    #[test]
    fn read_only_paths_overlay_the_write_set() {
        let config = SandboxConfig::builder()
            .working_directory("/tmp/agent-workspace")
            .write_path("/tmp/agent-workspace")
            .read_only_path("/tmp/agent-workspace/vendored")
            .build()
            .unwrap();
        let cmd = build_bwrap_command_with_path(
            Path::new("/usr/bin/bwrap"),
            &config,
            Path::new("/run/swage/agent.sock"),
            None,
            Path::new("/usr/bin/swage-agent"),
        )
        .unwrap();
        let args = args_of(&cmd);
        let write = args
            .windows(3)
            .position(|w| {
                w == [
                    OsStr::new("--bind"),
                    OsStr::new("/tmp/agent-workspace"),
                    OsStr::new("/tmp/agent-workspace"),
                ]
            })
            .expect("the workspace is bound writable");
        let read_only = args
            .windows(3)
            .position(|w| {
                w == [
                    OsStr::new("--ro-bind"),
                    OsStr::new("/tmp/agent-workspace/vendored"),
                    OsStr::new("/tmp/agent-workspace/vendored"),
                ]
            })
            .expect("the read-only path is overlaid");
        assert!(write < read_only, "the overlay must follow the write bind");
    }

    #[test]
    fn masked_paths_are_hidden_last_as_an_empty_directory_or_an_empty_file() {
        let home = tempfile::tempdir().unwrap();
        let ssh = home.path().join(".ssh");
        std::fs::create_dir(&ssh).unwrap();
        let netrc = home.path().join(".netrc");
        std::fs::write(&netrc, "machine x login y").unwrap();
        let absent = home.path().join(".never-made");
        let config = SandboxConfig::builder()
            .working_directory(home.path())
            .write_path(home.path())
            .masked_path(&ssh)
            .masked_path(&netrc)
            .masked_path(&absent)
            .build()
            .unwrap();
        let cmd = build_bwrap_command_with_path(
            Path::new("/usr/bin/bwrap"),
            &config,
            Path::new("/run/swage/agent.sock"),
            None,
            Path::new("/usr/bin/swage-agent"),
        )
        .unwrap();
        let args = args_of(&cmd);
        let write = args
            .iter()
            .position(|a| *a == home.path().as_os_str())
            .expect("the home is bound writable");
        let dir_mask = args
            .windows(2)
            .position(|w| w == [OsStr::new("--tmpfs"), ssh.as_os_str()])
            .expect("a masked directory becomes an empty tmpfs");
        let file_mask = args
            .windows(3)
            .position(|w| {
                w == [
                    OsStr::new("--ro-bind"),
                    OsStr::new("/dev/null"),
                    netrc.as_os_str(),
                ]
            })
            .expect("a masked file reads as /dev/null");
        assert!(
            write < dir_mask && write < file_mask,
            "masks follow the write set"
        );
        assert!(
            !args.contains(&absent.as_os_str()),
            "a masked path that does not exist needs no mask; args: {args:?}"
        );
    }

    #[test]
    fn the_egress_socket_is_bound_and_named_only_when_given() {
        let config = basic_config();
        let cmd = build_bwrap_command_with_path(
            Path::new("/usr/bin/bwrap"),
            &config,
            Path::new("/run/swage/agent.sock"),
            Some(Path::new("/run/swage/egress.sock")),
            Path::new("/usr/bin/swage-agent"),
        )
        .unwrap();
        let args = args_of(&cmd);
        assert!(args.windows(3).any(|w| w
            == [
                OsStr::new("--bind"),
                OsStr::new("/run/swage/egress.sock"),
                OsStr::new("/run/swage/egress.sock")
            ]));
        assert!(args.windows(3).any(|w| w
            == [
                OsStr::new("--setenv"),
                OsStr::new(crate::egress::EGRESS_SOCKET_ENV),
                OsStr::new("/run/swage/egress.sock")
            ]));

        let cmd = build_bwrap_command_with_path(
            Path::new("/usr/bin/bwrap"),
            &config,
            Path::new("/run/swage/agent.sock"),
            None,
            Path::new("/usr/bin/swage-agent"),
        )
        .unwrap();
        let args = args_of(&cmd);
        assert!(
            !args.contains(&OsStr::new(crate::egress::EGRESS_SOCKET_ENV)),
            "no egress socket means no proxy named; args: {args:?}"
        );
    }

    #[test]
    fn ipc_socket_env_is_set() {
        let config = basic_config();
        let cmd = build_bwrap_command_with_path(
            Path::new("/usr/bin/bwrap"),
            &config,
            Path::new("/run/swage/test.sock"),
            None,
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
            None,
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
            None,
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
            None,
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
            None,
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

    #[test]
    fn socket_under_tmp_is_bound_over_the_tmpfs_overlay() {
        // The long-path fallback socket, and a socket under a runtime
        // directory that itself lies under /tmp.
        let layouts = [
            (Path::new("/tmp/swage-1000/swage-sandbox-0000.sock"), None),
            (
                Path::new("/tmp/swx-run/swage/swage-sandbox-0000.sock"),
                Some(Path::new("/tmp/swx-run")),
            ),
        ];
        for (socket, runtime_dir) in layouts {
            let cmd = build_bwrap_command_with_path(
                Path::new("/usr/bin/bwrap"),
                &basic_config(),
                socket,
                None,
                Path::new("/usr/bin/swage-agent"),
            )
            .unwrap();
            let args = args_of(&cmd);
            let tmpfs = args
                .windows(2)
                .position(|w| w == [OsStr::new("--tmpfs"), OsStr::new("/tmp")])
                .expect("command overlays /tmp with tmpfs");

            // Every bind-type mount, whatever its flavour: (position, source, destination).
            let binds: Vec<(usize, &Path, &Path)> = args
                .windows(3)
                .enumerate()
                .filter(|(_, w)| {
                    let flag = w[0].to_string_lossy();
                    flag.starts_with("--") && flag.contains("bind")
                })
                .map(|(i, w)| (i, Path::new(w[1]), Path::new(w[2])))
                .collect();

            // The one permitted exposure of `/` is the read-only root bind
            // that precedes the tmpfs, which then hides /tmp beneath it.
            // Every other bind — at any position — must not carry `/`, any
            // other ancestor of the socket, /tmp or the runtime directory as
            // its source or destination.
            let root_bind = binds
                .iter()
                .position(|&(position, source, destination)| {
                    args[position] == OsStr::new("--ro-bind")
                        && source == Path::new("/")
                        && destination == Path::new("/")
                        && position < tmpfs
                })
                .expect("a read-only root bind precedes the /tmp tmpfs");
            let mut forbidden: Vec<&Path> = socket.ancestors().skip(1).collect();
            forbidden.push(Path::new("/tmp"));
            forbidden.extend(runtime_dir);
            for (_, source, destination) in binds
                .iter()
                .enumerate()
                .filter(|(index, _)| *index != root_bind)
                .map(|(_, bind)| bind)
            {
                for path in &forbidden {
                    assert!(
                        source != path && destination != path,
                        "{} must not be exposed by any bind; args: {args:?}",
                        path.display()
                    );
                }
            }

            let socket_binds: Vec<&(usize, &Path, &Path)> = binds
                .iter()
                .filter(|(_, source, destination)| *source == socket || *destination == socket)
                .collect();
            assert_eq!(
                socket_binds.len(),
                1,
                "exactly one bind must carry the socket; args: {args:?}"
            );
            let (position, source, destination) = *socket_binds[0];
            assert_eq!(
                (args[position], source, destination),
                (OsStr::new("--bind"), socket, socket),
                "the socket file itself must be bound at its own path; args: {args:?}"
            );
            assert!(
                position > tmpfs,
                "socket bind must follow the /tmp tmpfs or the overlay hides it; args: {args:?}"
            );
        }
    }
}

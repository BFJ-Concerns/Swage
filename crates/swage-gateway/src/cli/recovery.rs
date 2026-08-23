//! Recovery for gateways that are serving but have lost runtime metadata.
//!
//! The metadata file lives under the user's runtime directory. If it is
//! removed while the daemon keeps serving, lifecycle commands lose the PID and
//! the next `gateway start` can spawn a second process that immediately fails
//! to bind. Recovery treats the configured health endpoint as the source of
//! truth, then reconstructs enough metadata for normal status/stop/restart
//! handling to continue.

use std::{
    collections::HashSet,
    ffi::{OsStr, OsString},
    fs,
    net::{IpAddr, Ipv4Addr},
    path::{Path, PathBuf},
};

use swage_client::{http::GatewayClient, runtime::RuntimeMetadata};

const DEFAULT_HOST: &str = "127.0.0.1";
const DEFAULT_PORT: u16 = 3000;

#[derive(Debug, PartialEq, Eq)]
pub struct GatewayLifecycleEnvironment {
    swage_home: PathBuf,
    home: Option<PathBuf>,
    xdg_runtime_dir: Option<PathBuf>,
}

impl GatewayLifecycleEnvironment {
    pub fn swage_home(&self) -> &Path {
        &self.swage_home
    }

    pub fn metadata_path(&self) -> PathBuf {
        match &self.xdg_runtime_dir {
            Some(runtime_dir) => runtime_dir.join("swage").join("metadata.json"),
            None => self.swage_home.join("run").join("metadata.json"),
        }
    }

    pub fn apply_to_command(&self, command: &mut std::process::Command) {
        command.env("SWAGE_HOME", &self.swage_home);
        match &self.home {
            Some(home) => {
                command.env("HOME", home);
            }
            None => {
                command.env_remove("HOME");
            }
        }
        match &self.xdg_runtime_dir {
            Some(runtime_dir) => {
                command.env("XDG_RUNTIME_DIR", runtime_dir);
            }
            None => {
                command.env_remove("XDG_RUNTIME_DIR");
            }
        }
    }
}

pub fn recover_gateway_lifecycle_environment(
    pid: u32,
) -> Result<GatewayLifecycleEnvironment, String> {
    #[cfg(target_os = "linux")]
    {
        let environ = fs::read(format!("/proc/{pid}/environ"))
            .map_err(|error| format!("could not read /proc/{pid}/environ: {error}"))?;
        lifecycle_environment_from_bytes(&environ).ok_or_else(|| {
            format!(
                "gateway PID {pid} exposes neither SWAGE_HOME nor HOME; its data root cannot be determined"
            )
        })
    }

    #[cfg(not(target_os = "linux"))]
    {
        let _ = pid;
        Err("gateway data-root recovery is not supported on this platform".to_string())
    }
}

pub async fn recover_configured_gateway_metadata(
    host: Option<&str>,
    port: Option<u16>,
) -> Option<RuntimeMetadata> {
    recover_configured_gateway_metadata_at(host, port, &RuntimeMetadata::metadata_path()).await
}

pub async fn recover_configured_gateway_metadata_at(
    host: Option<&str>,
    port: Option<u16>,
    metadata_path: &Path,
) -> Option<RuntimeMetadata> {
    let host = host.unwrap_or(DEFAULT_HOST);
    let port = port.unwrap_or(DEFAULT_PORT);
    let health_host = health_host(host);
    let address = format!("http://{health_host}:{port}");

    if GatewayClient::new(&address).health().await.is_err() {
        return None;
    }

    let pid = listener_pid(host, port)?;
    if process_metadata_path(pid).as_deref() != Some(metadata_path) {
        return None;
    }
    let metadata = RuntimeMetadata::new(pid, address);
    if let Err(error) = metadata.save_to(metadata_path) {
        eprintln!("Gateway is running but runtime metadata could not be restored: {error}");
    }
    Some(metadata)
}

fn health_host(host: &str) -> &str {
    match host {
        "0.0.0.0" | "::" | "[::]" => DEFAULT_HOST,
        other => other,
    }
}

fn listener_pid(host: &str, port: u16) -> Option<u32> {
    #[cfg(target_os = "linux")]
    {
        let inodes = listener_socket_inodes(host, port);
        if inodes.is_empty() {
            return None;
        }
        pid_owning_any_socket_inode(&inodes)
    }

    #[cfg(not(target_os = "linux"))]
    {
        let _ = (host, port);
        None
    }
}

#[cfg(target_os = "linux")]
fn listener_socket_inodes(host: &str, port: u16) -> HashSet<String> {
    let mut inodes = HashSet::new();
    collect_listener_socket_inodes(Path::new("/proc/net/tcp"), host, port, &mut inodes);
    collect_listener_socket_inodes(Path::new("/proc/net/tcp6"), host, port, &mut inodes);
    inodes
}

#[cfg(target_os = "linux")]
fn collect_listener_socket_inodes(
    table_path: &Path,
    host: &str,
    port: u16,
    inodes: &mut HashSet<String>,
) {
    let Ok(contents) = fs::read_to_string(table_path) else {
        return;
    };

    for line in contents.lines().skip(1) {
        let fields = line.split_whitespace().collect::<Vec<_>>();
        if fields.len() <= 9 || fields[3] != "0A" {
            continue;
        }
        let Some((local_host, local_port)) = parse_proc_net_local_address(fields[1]) else {
            continue;
        };
        if local_port == port && host_matches(host, local_host) {
            inodes.insert(fields[9].to_string());
        }
    }
}

#[cfg(target_os = "linux")]
fn parse_proc_net_local_address(value: &str) -> Option<(IpAddr, u16)> {
    let (host_hex, port_hex) = value.split_once(':')?;
    let port = u16::from_str_radix(port_hex, 16).ok()?;

    if host_hex.len() == 8 {
        let raw = u32::from_str_radix(host_hex, 16).ok()?;
        return Some((IpAddr::V4(Ipv4Addr::from(raw.to_le_bytes())), port));
    }

    // IPv6 addresses in /proc/net/tcp6 are enough for wildcard detection here.
    if host_hex.len() == 32 && host_hex.chars().all(|value| value == '0') {
        return Some((IpAddr::V4(Ipv4Addr::UNSPECIFIED), port));
    }

    None
}

#[cfg(target_os = "linux")]
fn host_matches(configured_host: &str, listener_host: IpAddr) -> bool {
    if configured_host == "0.0.0.0" || configured_host == "::" || configured_host == "[::]" {
        return listener_host.is_unspecified();
    }

    configured_host
        .parse::<IpAddr>()
        .map(|configured| configured == listener_host)
        .unwrap_or(false)
}

#[cfg(target_os = "linux")]
fn pid_owning_any_socket_inode(inodes: &HashSet<String>) -> Option<u32> {
    let proc_entries = fs::read_dir("/proc").ok()?;
    for entry in proc_entries.flatten() {
        let file_name = entry.file_name();
        let Some(pid_text) = file_name.to_str() else {
            continue;
        };
        let Ok(pid) = pid_text.parse::<u32>() else {
            continue;
        };
        let fd_dir = entry.path().join("fd");
        let Ok(fd_entries) = fs::read_dir(fd_dir) else {
            continue;
        };
        for fd_entry in fd_entries.flatten() {
            let Ok(target) = fs::read_link(fd_entry.path()) else {
                continue;
            };
            let target = target.to_string_lossy();
            if let Some(inode) = target
                .strip_prefix("socket:[")
                .and_then(|value| value.strip_suffix(']'))
                && inodes.contains(inode)
            {
                return Some(pid);
            }
        }
    }
    None
}

#[cfg(target_os = "linux")]
fn process_metadata_path(pid: u32) -> Option<PathBuf> {
    let environ = fs::read(format!("/proc/{pid}/environ")).ok()?;
    let environment = lifecycle_environment_from_bytes(&environ)?;

    Some(match environment.xdg_runtime_dir {
        Some(xdg_runtime_dir) => xdg_runtime_dir.join("swage").join("metadata.json"),
        None => environment.swage_home.join("run").join("metadata.json"),
    })
}

#[cfg(not(target_os = "linux"))]
fn process_metadata_path(_pid: u32) -> Option<PathBuf> {
    None
}

#[cfg(target_os = "linux")]
fn lifecycle_environment_from_bytes(environ: &[u8]) -> Option<GatewayLifecycleEnvironment> {
    use std::os::unix::ffi::{OsStrExt, OsStringExt};

    let mut home = None;
    let mut swage_home = None;
    let mut swage_home_was_present = false;
    let mut xdg_runtime_dir = None;

    for entry in environ.split(|byte| *byte == 0) {
        let Some(separator) = entry.iter().position(|byte| *byte == b'=') else {
            continue;
        };
        let key = OsStr::from_bytes(&entry[..separator]);
        let value = OsString::from_vec(entry[separator + 1..].to_vec());
        if key == "HOME" {
            home = Some(PathBuf::from(value));
        } else if key == "SWAGE_HOME" {
            swage_home_was_present = true;
            if !value.is_empty() {
                swage_home = Some(PathBuf::from(value));
            }
        } else if key == "XDG_RUNTIME_DIR" && !value.is_empty() {
            xdg_runtime_dir = Some(PathBuf::from(value));
        }
    }

    if swage_home_was_present && swage_home.is_none() {
        return None;
    }
    let root = swage_home.or_else(|| {
        home.as_ref()
            .filter(|home| !home.as_os_str().is_empty())
            .map(|home| home.join(".swage"))
    })?;
    Some(GatewayLifecycleEnvironment {
        swage_home: root,
        home,
        xdg_runtime_dir,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "linux")]
    #[test]
    fn parses_ipv4_proc_net_address() {
        let parsed = parse_proc_net_local_address("0100007F:0BB8").unwrap();
        assert_eq!(parsed, (IpAddr::V4(Ipv4Addr::LOCALHOST), 3000));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn host_matching_respects_wildcard_and_exact_addresses() {
        assert!(host_matches("127.0.0.1", IpAddr::V4(Ipv4Addr::LOCALHOST)));
        assert!(host_matches("0.0.0.0", IpAddr::V4(Ipv4Addr::UNSPECIFIED)));
        assert!(!host_matches(
            "127.0.0.1",
            IpAddr::V4(Ipv4Addr::UNSPECIFIED)
        ));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn lifecycle_environment_preserves_explicit_root_and_runtime_directory() {
        let environment = lifecycle_environment_from_bytes(
            b"HOME=/operator\0SWAGE_HOME=/data/swage\0XDG_RUNTIME_DIR=/run/user/1000\0",
        )
        .unwrap();

        assert_eq!(environment.swage_home, PathBuf::from("/data/swage"));
        assert_eq!(environment.home, Some(PathBuf::from("/operator")));
        assert_eq!(
            environment.xdg_runtime_dir,
            Some(PathBuf::from("/run/user/1000"))
        );

        let mut command = std::process::Command::new("swage");
        environment.apply_to_command(&mut command);
        let variables = command.get_envs().collect::<Vec<_>>();
        assert!(variables.contains(&(OsStr::new("SWAGE_HOME"), Some(OsStr::new("/data/swage")))));
        assert!(variables.contains(&(OsStr::new("HOME"), Some(OsStr::new("/operator")))));
        assert!(variables.contains(&(
            OsStr::new("XDG_RUNTIME_DIR"),
            Some(OsStr::new("/run/user/1000"))
        )));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn lifecycle_environment_materialises_the_effective_default_root() {
        let environment = lifecycle_environment_from_bytes(b"HOME=/operator\0").unwrap();

        assert_eq!(environment.swage_home, PathBuf::from("/operator/.swage"));
        assert_eq!(environment.home, Some(PathBuf::from("/operator")));
        assert_eq!(environment.xdg_runtime_dir, None);

        let mut command = std::process::Command::new("swage");
        environment.apply_to_command(&mut command);
        assert!(
            command
                .get_envs()
                .any(|entry| entry == (OsStr::new("XDG_RUNTIME_DIR"), None))
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn lifecycle_environment_rejects_unknown_data_root() {
        assert_eq!(lifecycle_environment_from_bytes(b"PATH=/usr/bin\0"), None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn lifecycle_environment_rejects_empty_explicit_root() {
        assert_eq!(
            lifecycle_environment_from_bytes(b"HOME=/operator\0SWAGE_HOME=\0"),
            None
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn lifecycle_environment_removes_home_when_the_gateway_did_not_have_one() {
        let environment = lifecycle_environment_from_bytes(b"SWAGE_HOME=/data/swage\0").unwrap();

        assert_eq!(environment.home, None);

        let mut command = std::process::Command::new("swage");
        environment.apply_to_command(&mut command);
        assert!(
            command
                .get_envs()
                .any(|entry| entry == (OsStr::new("HOME"), None))
        );
    }
}

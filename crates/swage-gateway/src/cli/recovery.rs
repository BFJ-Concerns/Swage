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
    fs,
    net::{IpAddr, Ipv4Addr},
    path::Path,
};

use swage_client::{http::GatewayClient, runtime::RuntimeMetadata};

const DEFAULT_HOST: &str = "127.0.0.1";
const DEFAULT_PORT: u16 = 3000;

pub async fn recover_configured_gateway_metadata(
    host: Option<&str>,
    port: Option<u16>,
) -> Option<RuntimeMetadata> {
    let host = host.unwrap_or(DEFAULT_HOST);
    let port = port.unwrap_or(DEFAULT_PORT);
    let health_host = health_host(host);
    let address = format!("http://{health_host}:{port}");

    if GatewayClient::new(&address).health().await.is_err() {
        return None;
    }

    let pid = listener_pid(host, port)?;
    if !pid_uses_current_metadata_path(pid) {
        return None;
    }
    let metadata = RuntimeMetadata::new(pid, address);
    if let Err(error) = metadata.save() {
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

fn pid_uses_current_metadata_path(pid: u32) -> bool {
    #[cfg(target_os = "linux")]
    {
        process_metadata_path(pid).is_some_and(|path| path == RuntimeMetadata::metadata_path())
    }

    #[cfg(not(target_os = "linux"))]
    {
        let _ = pid;
        false
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
fn process_metadata_path(pid: u32) -> Option<std::path::PathBuf> {
    let environ = fs::read(format!("/proc/{pid}/environ")).ok()?;
    let mut home = None;
    let mut swage_home = None;
    let mut xdg_runtime_dir = None;

    for entry in environ.split(|byte| *byte == 0) {
        let text = std::str::from_utf8(entry).ok()?;
        if let Some(value) = text.strip_prefix("HOME=") {
            home = Some(value.to_string());
        } else if let Some(value) = text.strip_prefix("SWAGE_HOME=") {
            swage_home = Some(value.to_string());
        } else if let Some(value) = text.strip_prefix("XDG_RUNTIME_DIR=") {
            xdg_runtime_dir = Some(value.to_string());
        }
    }

    if let Some(xdg_runtime_dir) = xdg_runtime_dir {
        return Some(
            std::path::PathBuf::from(xdg_runtime_dir)
                .join("swage")
                .join("metadata.json"),
        );
    }

    let root = swage_home
        .map(std::path::PathBuf::from)
        .or_else(|| home.map(|home| std::path::PathBuf::from(home).join(".swage")))?;
    Some(root.join("run").join("metadata.json"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "linux")]
    #[test]
    fn parses_ipv4_proc_net_address() {
        let parsed = parse_proc_net_local_address("0100007F:0BB8").unwrap();
        assert_eq!(parsed, (IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 3000));
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
}

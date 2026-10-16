//! Process-level sandboxing for agent execution.
//!
//! Provides platform-abstracted isolation using bubblewrap and Landlock on Linux,
//! with a fallback mode for unsupported platforms. Tool calls are dispatched and
//! LLM requests are proxied through the host via IPC — credentials never enter
//! the agent process.

use std::path::PathBuf;

use swage_core::SandboxError;

mod agent_loop;
mod bubblewrap;
mod config;
mod fallback;
pub mod ipc;
mod landlock;
pub mod llm_handler;
mod process;
mod provider;
mod sandbox;
mod socket;

pub(crate) const SANDBOX_AGENT_BINARY_ENV: &str = "SWAGE_SANDBOX_AGENT_BINARY";

#[cfg(test)]
pub(crate) static SANDBOX_TEST_ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

pub(crate) fn resolve_agent_binary() -> Result<PathBuf, SandboxError> {
    if let Some(path) = std::env::var_os(SANDBOX_AGENT_BINARY_ENV) {
        return Ok(PathBuf::from(path));
    }

    std::env::current_exe().map_err(|e| SandboxError::SpawnFailed {
        message: format!("cannot determine agent binary path: {e}"),
    })
}

pub use agent_loop::{apply_child_landlock_from_env, run_agent_loop};

/// The binary that re-executes as the sandboxed agent and as the governor
/// of a sandboxed program: the host's own, or `SWAGE_SANDBOX_AGENT_BINARY`.
pub fn host_binary() -> Result<PathBuf, SandboxError> {
    resolve_agent_binary()
}

/// The environment variable that carries the sandbox config to a child.
pub const SANDBOX_CONFIG_ENV: &str = "SWAGE_SANDBOX_CONFIG_JSON";

/// The command that runs `program` with `args` under the Landlock rules
/// `config` describes and nothing else: the host binary re-executes as the
/// governor (`--sandboxed-exec`), applies the rules from the environment and
/// execs the program. For a turn whose sandbox is the permitted fallback —
/// the agent's own boundary there.
pub fn governed_command(
    config: &SandboxConfig,
    program: &std::path::Path,
    args: &[String],
) -> Result<tokio::process::Command, SandboxError> {
    let host = host_binary()?;
    let config_json = serde_json::to_string(config).map_err(|error| SandboxError::SpawnFailed {
        message: format!("failed to serialise sandbox config for the governed program: {error}"),
    })?;
    let mut command = tokio::process::Command::new(host);
    command.arg("--sandboxed-exec").arg(program).args(args);
    // A clean environment: the rules, a path for the program's interpreter,
    // and what the caller adds — never the gateway's own variables.
    command.env_clear();
    if let Some(path) = std::env::var_os("PATH") {
        command.env("PATH", path);
    }
    command.env(SANDBOX_CONFIG_ENV, config_json);
    if let Some(root) = program.parent() {
        command.current_dir(root);
    }
    Ok(command)
}
pub use bubblewrap::BubblewrapSandbox;
pub use bubblewrap::build_bwrap_command_for;
pub use bubblewrap::find_bwrap;
pub use config::{SandboxConfig, SandboxConfigError};
pub use fallback::FallbackSandbox;
pub use process::{ChildProcessManager, cleanup_failed_startup_process};
pub use provider::IpcCompletionProvider;
pub use sandbox::{Sandbox, ToolCallResult};
pub use socket::SocketManager;

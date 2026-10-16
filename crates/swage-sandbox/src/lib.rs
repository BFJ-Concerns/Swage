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

pub use agent_loop::{child_sandbox_config_from_env, run_agent_loop};
pub use bubblewrap::BubblewrapSandbox;
pub use bubblewrap::find_bwrap;
pub use config::{SandboxConfig, SandboxConfigError};
pub use fallback::FallbackSandbox;
pub use process::{ChildProcessManager, cleanup_failed_startup_process};
pub use provider::IpcCompletionProvider;
pub use sandbox::{Sandbox, ToolCallResult};
pub use socket::SocketManager;

//! Per-agent sandbox configuration.

use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use thiserror::Error;

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Errors that can occur when building a [`SandboxConfig`].
#[derive(Debug, Error)]
pub enum SandboxConfigError {
    /// A required field was not provided.
    #[error("required field not set: {field}")]
    MissingField { field: &'static str },
}

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

/// Per-agent sandbox configuration.
///
/// Configures isolation boundaries, mount points, Landlock filesystem rules,
/// and timeout parameters for a sandboxed agent process.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxConfig {
    /// The agent's working directory — read-write inside the sandbox.
    pub working_directory: PathBuf,
    /// Additional read-only bind mounts (e.g., topic source, shared libraries).
    pub read_only_mounts: Vec<PathBuf>,
    /// Additional read-write bind mounts (e.g., output directory).
    pub read_write_mounts: Vec<PathBuf>,
    /// Landlock-enforced read paths (kernel-level filesystem access control).
    pub landlock_read_paths: Vec<PathBuf>,
    /// Landlock-enforced write paths (kernel-level filesystem access control).
    pub landlock_write_paths: Vec<PathBuf>,
    /// Time to wait for graceful shutdown before SIGKILL.
    pub shutdown_grace_period: Duration,
    /// Per-tool-call IPC timeout.
    pub tool_call_timeout: Duration,
    /// Session tool settings carried to the child through the sandbox configuration.
    pub tool_runtime: Option<swage_protocol::client::settings::ToolRuntimeSettings>,
    /// Per-LLM-request IPC timeout.
    pub llm_request_timeout: Duration,
}

impl SandboxConfig {
    /// Returns a builder for constructing a [`SandboxConfig`].
    pub fn builder() -> SandboxConfigBuilder {
        SandboxConfigBuilder::new()
    }
}

// ---------------------------------------------------------------------------
// Builder
// ---------------------------------------------------------------------------

/// Builder for [`SandboxConfig`].
///
/// Provides sensible defaults for optional fields. The `working_directory`
/// must be set explicitly — there is no safe default for where the agent runs.
pub struct SandboxConfigBuilder {
    working_directory: Option<PathBuf>,
    read_only_mounts: Vec<PathBuf>,
    read_write_mounts: Vec<PathBuf>,
    landlock_read_paths: Vec<PathBuf>,
    landlock_write_paths: Vec<PathBuf>,
    // 5 second default — long enough for a clean shutdown, short enough not
    // to stall the host on a hung agent.
    shutdown_grace_period: Duration,
    // 30 second default — tools that run external commands need headroom.
    tool_call_timeout: Duration,
    tool_runtime: Option<swage_protocol::client::settings::ToolRuntimeSettings>,
    // 60 second default — LLM requests can take a while for large completions.
    llm_request_timeout: Duration,
}

impl SandboxConfigBuilder {
    /// Creates a new builder with all defaults applied.
    pub fn new() -> Self {
        Self {
            working_directory: None,
            read_only_mounts: Vec::new(),
            read_write_mounts: Vec::new(),
            landlock_read_paths: Vec::new(),
            landlock_write_paths: Vec::new(),
            shutdown_grace_period: Duration::from_secs(5),
            tool_call_timeout: Duration::from_secs(30),
            tool_runtime: None,
            llm_request_timeout: Duration::from_secs(60),
        }
    }

    /// Sets the agent's working directory (required).
    pub fn working_directory(mut self, path: impl Into<PathBuf>) -> Self {
        self.working_directory = Some(path.into());
        self
    }

    /// Adds a read-only bind mount into the sandbox.
    pub fn read_only_mount(mut self, path: impl Into<PathBuf>) -> Self {
        self.read_only_mounts.push(path.into());
        self
    }

    /// Adds a read-write bind mount into the sandbox.
    pub fn read_write_mount(mut self, path: impl Into<PathBuf>) -> Self {
        self.read_write_mounts.push(path.into());
        self
    }

    /// Adds a Landlock-enforced read path (kernel filesystem access control).
    pub fn landlock_read_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.landlock_read_paths.push(path.into());
        self
    }

    /// Adds a Landlock-enforced write path (kernel filesystem access control).
    pub fn landlock_write_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.landlock_write_paths.push(path.into());
        self
    }

    /// Overrides the graceful shutdown timeout (default: 5s).
    pub fn shutdown_grace_period(mut self, duration: Duration) -> Self {
        self.shutdown_grace_period = duration;
        self
    }

    /// Overrides the per-tool-call IPC timeout (default: 30s).
    pub fn tool_call_timeout(mut self, duration: Duration) -> Self {
        self.tool_call_timeout = duration;
        self
    }

    /// Pass the session's existing tool settings to the child process.
    pub fn tool_runtime(
        mut self,
        settings: swage_protocol::client::settings::ToolRuntimeSettings,
    ) -> Self {
        self.tool_runtime = Some(settings);
        self
    }

    /// Overrides the per-LLM-request IPC timeout (default: 60s).
    pub fn llm_request_timeout(mut self, duration: Duration) -> Self {
        self.llm_request_timeout = duration;
        self
    }

    /// Validates the builder state and returns a [`SandboxConfig`].
    ///
    /// Fails if `working_directory` was not set.
    pub fn build(self) -> Result<SandboxConfig, SandboxConfigError> {
        let working_directory = self
            .working_directory
            .ok_or(SandboxConfigError::MissingField {
                field: "working_directory",
            })?;

        Ok(SandboxConfig {
            working_directory,
            read_only_mounts: self.read_only_mounts,
            read_write_mounts: self.read_write_mounts,
            landlock_read_paths: self.landlock_read_paths,
            landlock_write_paths: self.landlock_write_paths,
            shutdown_grace_period: self.shutdown_grace_period,
            // The shell owns its deadline. Allow its process cleanup and reply
            // to finish before treating silence as a broken IPC connection.
            tool_call_timeout: self.tool_runtime.as_ref().map_or(
                self.tool_call_timeout,
                |settings| {
                    self.tool_call_timeout.max(
                        Duration::from_secs(settings.shell_timeout_secs)
                            .saturating_add(self.shutdown_grace_period),
                    )
                },
            ),
            tool_runtime: self.tool_runtime,
            llm_request_timeout: self.llm_request_timeout,
        })
    }
}

impl Default for SandboxConfigBuilder {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    // -- Build succeeds -------------------------------------------------------

    #[test]
    fn build_with_working_directory_succeeds() {
        let config = SandboxConfig::builder()
            .working_directory("/tmp/agent-workspace")
            .build()
            .expect("build should succeed when working_directory is set");

        assert_eq!(config.working_directory, Path::new("/tmp/agent-workspace"));
    }

    // -- Default values -------------------------------------------------------

    #[test]
    fn defaults_are_sensible() {
        let config = SandboxConfig::builder()
            .working_directory("/tmp/agent")
            .build()
            .unwrap();

        assert_eq!(config.shutdown_grace_period, Duration::from_secs(5));
        assert_eq!(config.tool_call_timeout, Duration::from_secs(30));
        assert_eq!(config.llm_request_timeout, Duration::from_secs(60));
        assert!(config.read_only_mounts.is_empty());
        assert!(config.read_write_mounts.is_empty());
        assert!(config.landlock_read_paths.is_empty());
        assert!(config.landlock_write_paths.is_empty());
    }

    // -- Override fields -------------------------------------------------------

    #[test]
    fn override_shutdown_grace_period() {
        let config = SandboxConfig::builder()
            .working_directory("/tmp/agent")
            .shutdown_grace_period(Duration::from_secs(10))
            .build()
            .unwrap();

        assert_eq!(config.shutdown_grace_period, Duration::from_secs(10));
    }

    #[test]
    fn override_tool_call_timeout() {
        let config = SandboxConfig::builder()
            .working_directory("/tmp/agent")
            .tool_call_timeout(Duration::from_secs(15))
            .build()
            .unwrap();

        assert_eq!(config.tool_call_timeout, Duration::from_secs(15));
    }

    #[test]
    fn override_llm_request_timeout() {
        let config = SandboxConfig::builder()
            .working_directory("/tmp/agent")
            .llm_request_timeout(Duration::from_secs(120))
            .build()
            .unwrap();

        assert_eq!(config.llm_request_timeout, Duration::from_secs(120));
    }

    #[test]
    fn add_read_only_mounts() {
        let config = SandboxConfig::builder()
            .working_directory("/tmp/agent")
            .read_only_mount("/usr/lib")
            .read_only_mount("/etc/ssl")
            .build()
            .unwrap();

        assert_eq!(config.read_only_mounts.len(), 2);
        assert!(config.read_only_mounts.contains(&PathBuf::from("/usr/lib")));
        assert!(config.read_only_mounts.contains(&PathBuf::from("/etc/ssl")));
    }

    #[test]
    fn add_read_write_mounts() {
        let config = SandboxConfig::builder()
            .working_directory("/tmp/agent")
            .read_write_mount("/tmp/output")
            .build()
            .unwrap();

        assert_eq!(config.read_write_mounts.len(), 1);
        assert_eq!(config.read_write_mounts[0], PathBuf::from("/tmp/output"));
    }

    #[test]
    fn add_landlock_paths() {
        let config = SandboxConfig::builder()
            .working_directory("/tmp/agent")
            .landlock_read_path("/home/user/topic")
            .landlock_write_path("/tmp/output")
            .build()
            .unwrap();

        assert_eq!(config.landlock_read_paths.len(), 1);
        assert_eq!(
            config.landlock_read_paths[0],
            PathBuf::from("/home/user/topic")
        );
        assert_eq!(config.landlock_write_paths.len(), 1);
        assert_eq!(config.landlock_write_paths[0], PathBuf::from("/tmp/output"));
    }

    // -- Validation -----------------------------------------------------------

    #[test]
    fn build_fails_without_working_directory() {
        let result = SandboxConfig::builder().build();

        match result {
            Err(SandboxConfigError::MissingField { field }) => {
                assert_eq!(field, "working_directory");
            }
            Ok(_) => panic!("expected Err(MissingField), but build succeeded"),
        }
    }

    // -- Clone + Debug --------------------------------------------------------

    #[test]
    fn config_is_clone() {
        let config = SandboxConfig::builder()
            .working_directory("/tmp/agent")
            .read_only_mount("/usr/lib")
            .build()
            .unwrap();

        let clone = config.clone();
        assert_eq!(clone.working_directory, config.working_directory);
        assert_eq!(clone.read_only_mounts, config.read_only_mounts);
    }

    #[test]
    fn config_is_debug() {
        let config = SandboxConfig::builder()
            .working_directory("/tmp/agent")
            .build()
            .unwrap();

        let debug = format!("{config:?}");
        assert!(debug.contains("SandboxConfig"));
        assert!(debug.contains("agent"));
    }
}

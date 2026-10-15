//! Per-turn sandbox policy: what the sandboxed process may read, write and
//! never see.
//!
//! The policy reads broadly and writes narrowly. Everything the host can see
//! is readable inside the sandbox except the private paths the policy masks
//! — key material, cloud credentials, the gateway's own data root — which
//! read as empty. Writes are limited to a declared set: the workspace when
//! its grant allows, the workspace's git metadata where it lies outside the
//! tree, the shared build and package caches, and any attachment granted
//! write. A path inside that set can still be held read-only. The result is
//! that ordinary development work — build, commit, push, fetch — needs no
//! per-topic configuration, and the things worth protecting are protected by
//! default.

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

/// Per-turn sandbox policy and timeouts.
///
/// Serialised to the sandboxed child, which applies the write set and the
/// broad read through Landlock; the host applies the same policy through
/// bubblewrap's mounts, where the masks and the read-only paths are enforced.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxConfig {
    /// The directory the agent's commands run in. Writable only when it is
    /// also in `write_paths`.
    pub working_directory: PathBuf,
    /// Paths the sandboxed process may write. Everything else it can see is
    /// read-only.
    pub write_paths: Vec<PathBuf>,
    /// Paths kept read-only even where a write path above them would allow
    /// writing: a read-only attachment inside a writable tree, say.
    pub read_only_paths: Vec<PathBuf>,
    /// Private paths hidden from the sandbox altogether. A directory reads as
    /// empty; a file reads as empty. Enforced by the mount layer, so the
    /// fallback backend cannot honour them.
    pub masked_paths: Vec<PathBuf>,
    /// Time to wait for graceful shutdown before SIGKILL.
    pub shutdown_grace_period: Duration,
    /// Per-tool-call IPC timeout.
    pub tool_call_timeout: Duration,
    /// Per-LLM-request IPC timeout.
    pub llm_request_timeout: Duration,
}

impl SandboxConfig {
    /// Returns a builder for constructing a [`SandboxConfig`].
    pub fn builder() -> SandboxConfigBuilder {
        SandboxConfigBuilder::new()
    }

    /// Whether the working directory is in the write set.
    pub fn working_directory_is_writable(&self) -> bool {
        self.write_paths
            .iter()
            .any(|path| path == &self.working_directory)
    }
}

// ---------------------------------------------------------------------------
// Builder
// ---------------------------------------------------------------------------

/// Builder for [`SandboxConfig`].
///
/// Provides sensible defaults for optional fields. The `working_directory`
/// must be set explicitly — there is no safe default for where the agent runs
/// — and is read-only until `write_path` names it.
pub struct SandboxConfigBuilder {
    working_directory: Option<PathBuf>,
    write_paths: Vec<PathBuf>,
    read_only_paths: Vec<PathBuf>,
    masked_paths: Vec<PathBuf>,
    // 5 second default — long enough for a clean shutdown, short enough not
    // to stall the host on a hung agent.
    shutdown_grace_period: Duration,
    // 30 second default — tools that run external commands need headroom.
    tool_call_timeout: Duration,
    // 60 second default — LLM requests can take a while for large completions.
    llm_request_timeout: Duration,
}

impl SandboxConfigBuilder {
    /// Creates a new builder with all defaults applied.
    pub fn new() -> Self {
        Self {
            working_directory: None,
            write_paths: Vec::new(),
            read_only_paths: Vec::new(),
            masked_paths: Vec::new(),
            shutdown_grace_period: Duration::from_secs(5),
            tool_call_timeout: Duration::from_secs(30),
            llm_request_timeout: Duration::from_secs(60),
        }
    }

    /// Sets the agent's working directory (required).
    pub fn working_directory(mut self, path: impl Into<PathBuf>) -> Self {
        self.working_directory = Some(path.into());
        self
    }

    /// Adds a path the sandboxed process may write. Duplicates are kept once.
    pub fn write_path(mut self, path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        if !self.write_paths.contains(&path) {
            self.write_paths.push(path);
        }
        self
    }

    /// Adds a path held read-only even inside a writable tree.
    pub fn read_only_path(mut self, path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        if !self.read_only_paths.contains(&path) {
            self.read_only_paths.push(path);
        }
        self
    }

    /// Adds a private path the sandbox never sees.
    pub fn masked_path(mut self, path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        if !self.masked_paths.contains(&path) {
            self.masked_paths.push(path);
        }
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
            write_paths: self.write_paths,
            read_only_paths: self.read_only_paths,
            masked_paths: self.masked_paths,
            shutdown_grace_period: self.shutdown_grace_period,
            tool_call_timeout: self.tool_call_timeout,
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

    #[test]
    fn build_with_working_directory_succeeds() {
        let config = SandboxConfig::builder()
            .working_directory("/tmp/agent-workspace")
            .build()
            .expect("build should succeed when working_directory is set");

        assert_eq!(config.working_directory, Path::new("/tmp/agent-workspace"));
    }

    #[test]
    fn the_working_directory_is_read_only_until_named_as_a_write_path() {
        let read_only = SandboxConfig::builder()
            .working_directory("/tmp/agent")
            .build()
            .unwrap();
        assert!(!read_only.working_directory_is_writable());
        assert!(read_only.write_paths.is_empty());
        assert!(read_only.read_only_paths.is_empty());
        assert!(read_only.masked_paths.is_empty());

        let writable = SandboxConfig::builder()
            .working_directory("/tmp/agent")
            .write_path("/tmp/agent")
            .build()
            .unwrap();
        assert!(writable.working_directory_is_writable());
    }

    #[test]
    fn defaults_are_sensible() {
        let config = SandboxConfig::builder()
            .working_directory("/tmp/agent")
            .build()
            .unwrap();

        assert_eq!(config.shutdown_grace_period, Duration::from_secs(5));
        assert_eq!(config.tool_call_timeout, Duration::from_secs(30));
        assert_eq!(config.llm_request_timeout, Duration::from_secs(60));
    }

    #[test]
    fn timeouts_can_be_overridden() {
        let config = SandboxConfig::builder()
            .working_directory("/tmp/agent")
            .shutdown_grace_period(Duration::from_secs(10))
            .tool_call_timeout(Duration::from_secs(15))
            .llm_request_timeout(Duration::from_secs(120))
            .build()
            .unwrap();

        assert_eq!(config.shutdown_grace_period, Duration::from_secs(10));
        assert_eq!(config.tool_call_timeout, Duration::from_secs(15));
        assert_eq!(config.llm_request_timeout, Duration::from_secs(120));
    }

    #[test]
    fn each_path_set_is_kept_in_order_without_duplicates() {
        let config = SandboxConfig::builder()
            .working_directory("/tmp/agent")
            .write_path("/tmp/agent")
            .write_path("/home/u/.cache")
            .write_path("/tmp/agent")
            .read_only_path("/tmp/agent/vendored")
            .masked_path("/home/u/.ssh")
            .masked_path("/home/u/.ssh")
            .build()
            .unwrap();

        assert_eq!(
            config.write_paths,
            vec![PathBuf::from("/tmp/agent"), PathBuf::from("/home/u/.cache")]
        );
        assert_eq!(
            config.read_only_paths,
            vec![PathBuf::from("/tmp/agent/vendored")]
        );
        assert_eq!(config.masked_paths, vec![PathBuf::from("/home/u/.ssh")]);
    }

    #[test]
    fn build_fails_without_working_directory() {
        match SandboxConfig::builder().build() {
            Err(SandboxConfigError::MissingField { field }) => {
                assert_eq!(field, "working_directory");
            }
            Ok(_) => panic!("expected Err(MissingField), but build succeeded"),
        }
    }

    #[test]
    fn config_round_trips_through_json_for_the_child() {
        let config = SandboxConfig::builder()
            .working_directory("/tmp/agent")
            .write_path("/tmp/agent")
            .masked_path("/home/u/.ssh")
            .build()
            .unwrap();

        let json = serde_json::to_string(&config).unwrap();
        let decoded: SandboxConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.working_directory, config.working_directory);
        assert_eq!(decoded.write_paths, config.write_paths);
        assert_eq!(decoded.masked_paths, config.masked_paths);
    }
}

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

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use ts_rs::TS;

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Errors that can occur when building a [`SandboxPolicy`].
#[derive(Debug, Error)]
pub enum SandboxPolicyError {
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
#[derive(Debug, Clone, Serialize, Deserialize, TS, JsonSchema)]
#[ts(optional_fields = nullable)]
pub struct SandboxPolicy {
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
    /// Where tools keep their output artefacts, carried to the child
    /// independently of its working directory; writable, as a write path.
    pub tool_output_directory: Option<PathBuf>,
    /// Host-selected environment inherited by sandboxed commands.
    pub environment: std::collections::BTreeMap<String, String>,
    /// Time to wait for graceful shutdown before SIGKILL.
    #[ts(type = "{ secs: number, nanos: number }")]
    pub shutdown_grace_period: Duration,
    /// Per-tool-call IPC timeout.
    #[ts(type = "{ secs: number, nanos: number }")]
    pub tool_call_timeout: Duration,
    /// Per-LLM-request IPC timeout.
    #[ts(type = "{ secs: number, nanos: number }")]
    pub llm_request_timeout: Duration,
}

impl SandboxPolicy {
    /// Returns a builder for constructing a [`SandboxPolicy`].
    pub fn builder() -> SandboxPolicyBuilder {
        SandboxPolicyBuilder::new()
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

/// Builder for [`SandboxPolicy`].
///
/// Provides sensible defaults for optional fields. The `working_directory`
/// must be set explicitly — there is no safe default for where the agent runs
/// — and is read-only until `write_path` names it.
pub struct SandboxPolicyBuilder {
    working_directory: Option<PathBuf>,
    write_paths: Vec<PathBuf>,
    read_only_paths: Vec<PathBuf>,
    masked_paths: Vec<PathBuf>,
    tool_output_directory: Option<PathBuf>,
    // 5 second default — long enough for a clean shutdown, short enough not
    // to stall the host on a hung agent.
    shutdown_grace_period: Duration,
    // 30 second default — tools that run external commands need headroom.
    tool_call_timeout: Duration,
    // 60 second default — LLM requests can take a while for large completions.
    llm_request_timeout: Duration,
}

impl SandboxPolicyBuilder {
    /// Creates a new builder with all defaults applied.
    pub fn new() -> Self {
        Self {
            working_directory: None,
            write_paths: Vec::new(),
            read_only_paths: Vec::new(),
            masked_paths: Vec::new(),
            tool_output_directory: None,
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
    /// Sets the runtime-owned directory for tool-output artefacts. Add it as
    /// a write path too: the policy carries the location, not the grant.
    pub fn tool_output_directory(mut self, path: impl Into<PathBuf>) -> Self {
        self.tool_output_directory = Some(path.into());
        self
    }

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

    /// Validates the builder state and returns a [`SandboxPolicy`].
    ///
    /// Fails if `working_directory` was not set.
    pub fn build(self) -> Result<SandboxPolicy, SandboxPolicyError> {
        let working_directory = self
            .working_directory
            .ok_or(SandboxPolicyError::MissingField {
                field: "working_directory",
            })?;

        Ok(SandboxPolicy {
            working_directory,
            write_paths: self.write_paths,
            read_only_paths: self.read_only_paths,
            masked_paths: self.masked_paths,
            tool_output_directory: self.tool_output_directory,
            environment: std::collections::BTreeMap::new(),
            shutdown_grace_period: self.shutdown_grace_period,
            tool_call_timeout: self.tool_call_timeout,
            llm_request_timeout: self.llm_request_timeout,
        })
    }
}

impl Default for SandboxPolicyBuilder {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

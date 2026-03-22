//! Process-level sandboxing for agent execution.
//!
//! Provides platform-abstracted isolation using bubblewrap and Landlock on Linux,
//! with a fallback mode for unsupported platforms. Tool calls are dispatched and
//! LLM requests are proxied through the host via IPC — credentials never enter
//! the agent process.

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

pub use agent_loop::run_agent_loop;
pub use bubblewrap::BubblewrapSandbox;
pub use config::SandboxConfig;
pub use fallback::FallbackSandbox;
pub use provider::IpcCompletionProvider;
pub use sandbox::{Sandbox, ToolCallResult};

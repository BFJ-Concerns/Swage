//! # Swage
//!
//! Multi-agent LLM orchestration framework.
//!
//! Swage provides the building blocks for constructing autonomous agents that
//! converse with LLM backends, invoke tools, persist conversation history, and
//! run safely inside an isolated process. This crate is the single entry point:
//! everything exported here comes from the underlying workspace crates.
//!
//! ## Quick start
//!
//! ```rust,ignore
//! use std::sync::Arc;
//! use swage::{SwageBuilder, sqlite_storage, standard_leak_detector};
//! use swage::agent::InboundMessage;
//! use swage::storage::ProjectStore;
//!
//! // 1. Open storage (":memory:" for transient use, a file path for production).
//! let storage = Arc::new(sqlite_storage(":memory:")?);
//!
//! // 2. Build the agent — supply your LLM provider.
//! let mut agent = SwageBuilder::new()
//!     .provider(my_provider)
//!     .agent_id("my-agent")
//!     .storage(storage.clone())
//!     .leak_detector(standard_leak_detector())
//!     .build()?;
//!
//! // 3. Create a project and session, then send messages.
//! let project = ProjectStore::get_or_create_default(&*storage).await?;
//! let session_id = agent.create_session(project.id).await?;
//!
//! let response = agent.process_message(InboundMessage {
//!     message: "Hello, what can you do?".into(),
//!     session_id,
//!     thread_id: None,
//! }).await?;
//!
//! println!("{}", response.response);
//! # Ok::<(), swage::SwageError>(())
//! ```
//!
//! ## Module overview
//!
//! | Module | Purpose |
//! |--------|---------|
//! | [`core`] | Foundational types: messages, errors, request/response models, tool definitions |
//! | [`provider`] | [`provider::LlmProvider`] trait, OpenAI/Anthropic/Google implementations, reliability decorators |
//! | [`tool`] | Tool trait, registry, permission enforcement, rate limiting, dispatch pipeline |
//! | [`hook`] | Lifecycle hooks for intercepting agent events (session start, tool use, etc.) |
//! | [`safety`] | Credential leak detection and automatic redaction |
//! | [`storage`] | Persistence traits and SQLite backend; conversation, thread, and message models |
//! | [`agent`] | [`agent::Agent`] execution loop and [`agent::AgentBuilder`] |
//! | [`sandbox`] | Process-level isolation via bubblewrap + Landlock (Linux) or in-process fallback |
//! | [`config`] | Hierarchical TOML configuration (global → project → agent) |
//! | [`prelude`] | `use swage::prelude::*` — common imports for a minimal agent |
//!
//! ## High-level builder
//!
//! [`SwageBuilder`] is the recommended entry point. It wires all components
//! together with sensible defaults so you only need to supply a provider,
//! an agent ID, and storage.
//!
//! For finer control (custom tool registry, permission config, hooks), use
//! [`agent::AgentBuilder`] directly.
//!
//! ## Convenience constructors
//!
//! Three helpers cover the common setup path:
//!
//! - [`sqlite_storage`] — open or create a SQLite database
//! - [`standard_leak_detector`] — create a [`safety::LeakDetector`] with the
//!   default credential pattern library
//! - [`provider_with_reliability`] — wrap any provider in a retry +
//!   circuit-breaker stack

// Re-export modules.
pub mod agent;
pub mod builder;
pub mod config;
pub mod convenience;
pub mod core;
pub mod hook;
pub mod prelude;
pub mod provider;
pub mod safety;
pub mod sandbox;
pub mod storage;
pub mod tool;

// Migrated modules — re-exported from swage-core.
pub use swage_core::runtime;
pub use swage_core::secret;

// Re-export tracing for embedder convenience.
pub use tracing;

// ---------------------------------------------------------------------------
// Root-level re-exports — most-used types without a module qualifier.
// ---------------------------------------------------------------------------

pub use agent::Agent;
pub use builder::SwageBuilder;
pub use core::{ChatMessage, Role, SwageError};

// Convenience constructors at the crate root.
pub use convenience::{provider_with_reliability, sqlite_storage, standard_leak_detector};

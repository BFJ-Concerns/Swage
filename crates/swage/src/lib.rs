//! Swage: Multi-agent LLM orchestration framework
//!
//! Core library providing trait abstractions for LLM providers, storage,
//! tools, and secret management.
//!
//! # Logging
//!
//! This library uses the [`tracing`] crate for structured logging. All internal
//! operations emit events and spans to provide visibility into runtime behaviour.
//!
//! **Embedders must configure a tracing subscriber.** By default, no log output
//! is produced. Without subscriber initialisation, tracing events are discarded.
//!
//! # Examples
//!
//! Basic subscriber setup for console output:
//!
//! ```rust,ignore
//! use tracing_subscriber;
//!
//! fn main() {
//!     // Initialise subscriber with INFO level filter
//!     tracing_subscriber::fmt()
//!         .with_max_level(tracing::Level::INFO)
//!         .init();
//!
//!     // Now Swage operations will emit log output
//!     // your application code here
//! }
//! ```

pub mod config;
pub mod error;
pub mod http;
pub mod message;
pub mod provider;
pub mod providers;
pub mod rate_limit;
pub mod registry;
pub mod storage;
pub mod tool;

// Migrated modules — re-exported from swage-core
pub use swage_core::runtime;
pub use swage_core::secret;

// Re-export tracing for embedder convenience
pub use tracing;

// Re-export provider implementations for convenience
pub use providers::{AnthropicProvider, CustomProvider, GoogleProvider, OpenAiProvider};

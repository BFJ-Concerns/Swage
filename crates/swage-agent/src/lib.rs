//! Agent runtime for the Swage framework.
//!
//! Provides the single-agent execution loop that ties together the LLM provider,
//! tool system, hooks, safety layer, and persistence into a coherent agent.

pub mod agent;
pub mod builder;
pub mod compaction;
pub mod config;
pub mod context;
pub mod error;
pub mod event;
pub mod extraction;
pub mod message;

// Public re-exports — the primary surface for consumers.
pub use agent::Agent;
pub use builder::AgentBuilder;
pub use config::{AgentConfig, CompactionThreshold};
pub use error::AgentError;
pub use event::AgentEvent;
pub use message::{ErrorResponse, InboundMessage, OutboundMessage, TurnRequest};

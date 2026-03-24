//! Agent runtime for the Swage framework.
//!
//! Provides the single-agent execution loop that ties together the LLM provider,
//! tool system, hooks, safety layer, and persistence into a coherent agent.

pub mod agent;
pub mod approval;
pub mod builder;
pub mod cancel;
pub mod compaction;
pub mod config;
pub mod context;
pub mod context_transfer;
pub mod delegation;
pub mod error;
pub mod event;
pub mod expiry;
pub mod extraction;
pub mod mailbox;
pub mod message;
pub mod router;
pub mod schema_contract;

// Public re-exports — the primary surface for consumers.
pub use agent::Agent;
pub use builder::AgentBuilder;
pub use config::{AgentConfig, CompactionThreshold};
pub use error::AgentError;
pub use event::AgentEvent;
pub use approval::{ApprovalChannelRegistrar, ApprovalDecision};
pub use message::{ErrorResponse, InboundMessage, OutboundMessage, TurnRequest};
